//! Stylesheet cascade: parse `<style>` text into rules once, index each rule by
//! its subject key (id / class / tag), and resolve the matching declarations for
//! an element by testing only the handful of candidate rules that share a key.
//!
//! This replaces the naive "run every selector against the whole tree" approach,
//! which is O(rules x nodes) and dominated render time on large pages (thousands
//! of rules). The indexed cascade is closer to how real browsers match: bucket
//! rules, gather candidates per element, then match and sort by specificity.

use obscura_dom::selector::{CompiledSelector, Matcher, SelectorKey};
use obscura_dom::tree::{DomTree, NodeId};
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use crate::LayoutStyle;

/// CSS media type used while selecting conditional author rules.
///
/// Screen is the normal live-page mode. PDF export temporarily selects Print
/// so `@media print` and media-gated stylesheet blocks can participate without
/// mutating the document or changing JavaScript's live screen environment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CssMediaType {
    #[default]
    Screen,
    Print,
}

/// The part of the tree whose selector match may change when a dependency on
/// one element changes. Multiple bits can be present for selectors which use
/// the same key in more than one compound.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InvalidationReaches(u8);

impl InvalidationReaches {
    pub const SELF: Self = Self(1 << 0);
    pub const DESCENDANTS: Self = Self(1 << 1);
    pub const SIBLINGS: Self = Self(1 << 2);
    /// The selector needs a correctness-first fallback which phase 2 must not
    /// narrow to a local traversal.
    pub const CONSERVATIVE: Self = Self(1 << 3);

    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// One compiled rule's dependency on a selector key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidationDependency {
    /// Stylesheet source order of the compiled rule.
    pub rule_order: usize,
    pub reaches: InvalidationReaches,
}

/// A cheap positive key from the compound which anchors one `:has()`.
///
/// This is only an early rejection filter. Compounds without a direct key
/// remain unkeyed rather than borrowing a key from `:is()`/`:not()`, whose
/// boolean structure cannot be represented by one key without false negatives.
#[derive(Clone, Debug, PartialEq, Eq)]
enum RelationalSelectorKey {
    Id(String),
    Class(String),
    Attribute(String),
    LocalName(String),
}

/// Invalidation metadata for one `:has()` occurrence.
///
/// Gecko models the selector inside `:has()` as an upward dependency chain
/// (parent/ancestors/previous siblings), then resumes the ordinary selector
/// path outside the anchor. Obscura stores the smaller information needed by
/// its whole-subtree cascade: an optional anchor key, the outward reach, and
/// whether a key-independent child-list side effect can change the match.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RelationalInvalidation {
    pub rule_order: usize,
    anchor_key: Option<RelationalSelectorKey>,
    relative_keys: Vec<RelationalSelectorKey>,
    pub anchor_reaches: InvalidationReaches,
    pub unkeyed_subject: bool,
    pub sibling_side_effect: bool,
    pub structural_side_effect: bool,
    pub text_side_effect: bool,
    pub unrepresentable_outer_path: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StructuralInvalidation {
    pub rule_order: usize,
    pub state: String,
    subject_key: Option<RelationalSelectorKey>,
    pub reaches: InvalidationReaches,
    pub inside_relational: bool,
}

impl StructuralInvalidation {
    pub(crate) fn subject_may_match(&self, tree: &DomTree, node: NodeId) -> bool {
        relational_key_may_match(self.subject_key.as_ref(), tree, node)
    }
}

fn relational_key_may_match(
    key: Option<&RelationalSelectorKey>,
    tree: &DomTree,
    node: NodeId,
) -> bool {
    let Some(dom_node) = tree.get_node(node) else {
        return false;
    };
    if dom_node.as_element().is_none() {
        return false;
    }
    let quirks = tree.is_quirks();
    match key {
        None => true,
        Some(RelationalSelectorKey::Id(expected)) => dom_node
            .get_attribute("id")
            .is_some_and(|actual| {
                actual == expected || (quirks && actual.eq_ignore_ascii_case(expected))
            }),
        Some(RelationalSelectorKey::Class(expected)) => dom_node
            .get_attribute("class")
            .is_some_and(|classes| {
                classes.split_whitespace().any(|actual| {
                    actual == expected || (quirks && actual.eq_ignore_ascii_case(expected))
                })
            }),
        Some(RelationalSelectorKey::Attribute(expected)) => {
            dom_node.get_attribute(expected).is_some()
        }
        Some(RelationalSelectorKey::LocalName(expected)) => dom_node
            .as_element()
            .is_some_and(|element| element.local.as_ref().eq_ignore_ascii_case(expected)),
    }
}

impl RelationalInvalidation {
    pub(crate) fn anchor_may_match(&self, tree: &DomTree, node: NodeId) -> bool {
        relational_key_may_match(self.anchor_key.as_ref(), tree, node)
    }

    pub(crate) fn relative_path_may_match(&self, tree: &DomTree, node: NodeId) -> bool {
        let Some(dom_node) = tree.get_node(node) else {
            return false;
        };
        let Some(element) = dom_node.as_element() else {
            return false;
        };
        let quirks = tree.is_quirks();
        self.relative_keys.iter().any(|key| match key {
            RelationalSelectorKey::Id(expected) => dom_node
                .get_attribute("id")
                .is_some_and(|actual| {
                    actual == expected || (quirks && actual.eq_ignore_ascii_case(expected))
                }),
            RelationalSelectorKey::Class(expected) => dom_node
                .get_attribute("class")
                .is_some_and(|classes| {
                    classes.split_whitespace().any(|actual| {
                        actual == expected || (quirks && actual.eq_ignore_ascii_case(expected))
                    })
                }),
            RelationalSelectorKey::Attribute(expected) => dom_node.attrs().is_some_and(|attrs| {
                attrs
                    .iter()
                    .any(|attribute| attribute.name.local.as_ref().eq_ignore_ascii_case(expected))
            }),
            RelationalSelectorKey::LocalName(expected) => {
                element.local.as_ref().eq_ignore_ascii_case(expected)
            }
        })
    }
}

/// Selector dependencies retained alongside the compiled stylesheet.
///
/// This follows Gecko's conservative invalidation-map shape: live mutations
/// look up the changed id/class/attribute/local-name/state and receive one or
/// more traversal reaches. The renderer uses those reaches to retain clean
/// computed styles, with a full-cascade fallback for unrepresentable paths.
#[derive(Clone, Debug, Default)]
pub struct InvalidationMap {
    ids: HashMap<String, Vec<InvalidationDependency>>,
    classes: HashMap<String, Vec<InvalidationDependency>>,
    attributes: HashMap<String, Vec<InvalidationDependency>>,
    local_names: HashMap<String, Vec<InvalidationDependency>>,
    states: HashMap<String, Vec<InvalidationDependency>>,
    conservative_rule_orders: Vec<usize>,
    relational_rule_orders: Vec<usize>,
    unkeyed_relational_rule_orders: Vec<usize>,
    relational_invalidations: Vec<RelationalInvalidation>,
    structural_invalidations: Vec<StructuralInvalidation>,
    adjacent_sibling_selectors: bool,
    general_sibling_selectors: bool,
    unkeyed_sibling_selectors: bool,
}

impl InvalidationMap {
    pub fn id_dependencies(&self, id: &str) -> &[InvalidationDependency] {
        self.ids.get(id).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn class_dependencies(&self, class: &str) -> &[InvalidationDependency] {
        self.classes.get(class).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn attribute_dependencies(&self, attribute: &str) -> &[InvalidationDependency] {
        self.attributes
            .get(&attribute.to_ascii_lowercase())
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn local_name_dependencies(&self, local_name: &str) -> &[InvalidationDependency] {
        self.local_names
            .get(&local_name.to_ascii_lowercase())
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn state_dependencies(&self, state: &str) -> &[InvalidationDependency] {
        self.states
            .get(&state.to_ascii_lowercase())
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn conservative_rule_orders(&self) -> &[usize] {
        &self.conservative_rule_orders
    }

    pub fn requires_conservative_invalidation(&self) -> bool {
        !self.conservative_rule_orders.is_empty()
    }

    pub fn is_relational_rule(&self, rule_order: usize) -> bool {
        self.relational_rule_orders.contains(&rule_order)
    }

    pub fn has_unkeyed_relational_rules(&self) -> bool {
        !self.unkeyed_relational_rule_orders.is_empty()
    }

    pub(crate) fn relational_invalidations(&self) -> &[RelationalInvalidation] {
        &self.relational_invalidations
    }

    pub(crate) fn structural_invalidations<'a>(
        &'a self,
        state: &str,
    ) -> Vec<&'a StructuralInvalidation> {
        self.structural_invalidations
            .iter()
            .filter(move |invalidation| invalidation.state == state)
            .collect()
    }

    pub(crate) fn has_adjacent_sibling_selectors(&self) -> bool {
        self.adjacent_sibling_selectors
    }

    pub(crate) fn has_general_sibling_selectors(&self) -> bool {
        self.general_sibling_selectors
    }

    pub(crate) fn node_may_start_sibling_selector(
        &self,
        tree: &DomTree,
        node: NodeId,
    ) -> bool {
        if self.unkeyed_sibling_selectors
            || (tree.is_quirks()
                && (self.adjacent_sibling_selectors || self.general_sibling_selectors))
        {
            return true;
        }
        let Some(node) = tree.get_node(node) else {
            return false;
        };
        let reaches_sibling = |dependencies: &[InvalidationDependency]| {
            dependencies.iter().any(|dependency| {
                dependency.reaches.contains(InvalidationReaches::SIBLINGS)
            })
        };
        if node
            .get_attribute("id")
            .is_some_and(|id| reaches_sibling(self.id_dependencies(id)))
        {
            return true;
        }
        if node.get_attribute("class").is_some_and(|classes| {
            classes
                .split_whitespace()
                .any(|class| reaches_sibling(self.class_dependencies(class)))
        }) {
            return true;
        }
        if node.attrs().is_some_and(|attributes| {
            attributes.iter().any(|attribute| {
                reaches_sibling(self.attribute_dependencies(attribute.name.local.as_ref()))
            })
        }) {
            return true;
        }
        node.as_element().is_some_and(|element| {
            reaches_sibling(self.local_name_dependencies(element.local.as_ref()))
        })
    }

    pub fn dependency_count(&self) -> usize {
        self.ids
            .values()
            .chain(self.classes.values())
            .chain(self.attributes.values())
            .chain(self.local_names.values())
            .chain(self.states.values())
            .map(Vec::len)
            .sum()
    }

    fn push(
        map: &mut HashMap<String, Vec<InvalidationDependency>>,
        key: String,
        rule_order: usize,
        reaches: InvalidationReaches,
    ) {
        let dependencies = map.entry(key).or_default();
        if let Some(existing) = dependencies
            .iter_mut()
            .find(|dependency| dependency.rule_order == rule_order)
        {
            existing.reaches = existing.reaches.union(reaches);
        } else {
            dependencies.push(InvalidationDependency {
                rule_order,
                reaches,
            });
        }
    }

    fn push_id(&mut self, key: String, rule_order: usize, reaches: InvalidationReaches) {
        Self::push(&mut self.ids, key, rule_order, reaches);
    }

    fn push_class(&mut self, key: String, rule_order: usize, reaches: InvalidationReaches) {
        Self::push(&mut self.classes, key, rule_order, reaches);
    }

    fn push_attribute(&mut self, key: String, rule_order: usize, reaches: InvalidationReaches) {
        Self::push(
            &mut self.attributes,
            key.to_ascii_lowercase(),
            rule_order,
            reaches,
        );
    }

    fn push_local_name(&mut self, key: String, rule_order: usize, reaches: InvalidationReaches) {
        Self::push(
            &mut self.local_names,
            key.to_ascii_lowercase(),
            rule_order,
            reaches,
        );
    }

    fn push_state(&mut self, key: String, rule_order: usize, reaches: InvalidationReaches) {
        Self::push(
            &mut self.states,
            key.to_ascii_lowercase(),
            rule_order,
            reaches,
        );
    }

    fn mark_conservative(&mut self, rule_order: usize) {
        if self.conservative_rule_orders.last().copied() != Some(rule_order)
            && !self.conservative_rule_orders.contains(&rule_order)
        {
            self.conservative_rule_orders.push(rule_order);
        }
    }

    fn mark_relational(&mut self, rule_order: usize, unkeyed: bool) {
        if !self.relational_rule_orders.contains(&rule_order) {
            self.relational_rule_orders.push(rule_order);
        }
        if unkeyed && !self.unkeyed_relational_rule_orders.contains(&rule_order) {
            self.unkeyed_relational_rule_orders.push(rule_order);
        }
    }

    fn push_relational_invalidation(&mut self, invalidation: RelationalInvalidation) {
        if !self.relational_invalidations.contains(&invalidation) {
            self.relational_invalidations.push(invalidation);
        }
    }

    fn push_structural_invalidation(&mut self, invalidation: StructuralInvalidation) {
        if !self.structural_invalidations.contains(&invalidation) {
            self.structural_invalidations.push(invalidation);
        }
    }
}

fn compose_invalidation_reach(
    map: &mut InvalidationMap,
    inner: InvalidationReaches,
    outer: InvalidationReaches,
    rule_order: usize,
) -> InvalidationReaches {
    if outer == InvalidationReaches::SELF {
        inner
    } else if inner == InvalidationReaches::SELF || inner == outer {
        outer
    } else {
        // A sibling traversal nested under an ancestor traversal (or vice
        // versa) cannot be represented by the three simple phase-1 reaches.
        map.mark_conservative(rule_order);
        inner
            .union(outer)
            .union(InvalidationReaches::CONSERVATIVE)
    }
}

fn consume_css_identifier(chars: &[char], mut index: usize) -> (String, usize) {
    let mut value = String::new();
    while let Some(&ch) = chars.get(index) {
        if ch == '\\' {
            index += 1;
            let Some(&escaped) = chars.get(index) else {
                break;
            };
            if escaped.is_ascii_hexdigit() {
                let start = index;
                while index < chars.len()
                    && index - start < 6
                    && chars[index].is_ascii_hexdigit()
                {
                    index += 1;
                }
                let digits: String = chars[start..index].iter().collect();
                if let Ok(codepoint) = u32::from_str_radix(&digits, 16) {
                    value.push(char::from_u32(codepoint).unwrap_or('\u{fffd}'));
                }
                if chars.get(index).is_some_and(|ch| ch.is_whitespace()) {
                    index += 1;
                }
                continue;
            }
            value.push(escaped);
            index += 1;
            continue;
        }
        if ch.is_alphanumeric() || ch == '_' || ch == '-' || !ch.is_ascii() {
            value.push(ch);
            index += 1;
        } else {
            break;
        }
    }
    (value, index)
}

fn matching_delimiter(chars: &[char], open: usize, opening: char, closing: char) -> Option<usize> {
    let mut depth = 1usize;
    let mut quote = None;
    let mut index = open + 1;
    while index < chars.len() {
        let ch = chars[index];
        if ch == '\\' {
            index = (index + 2).min(chars.len());
            continue;
        }
        if let Some(active_quote) = quote {
            if ch == active_quote {
                quote = None;
            }
            index += 1;
            continue;
        }
        if ch == '\'' || ch == '"' {
            quote = Some(ch);
        } else if ch == opening {
            depth += 1;
        } else if ch == closing {
            depth -= 1;
            if depth == 0 {
                return Some(index);
            }
        }
        index += 1;
    }
    None
}

/// Split one complex selector into compounds and the traversal generated by a
/// dependency in each compound. Whitespace surrounding an explicit
/// combinator is not misread as an extra descendant combinator.
fn invalidation_compounds(selector: &str) -> (Vec<(String, InvalidationReaches)>, bool) {
    let chars: Vec<char> = selector.chars().collect();
    let mut compounds = Vec::new();
    let mut start = 0usize;
    let mut index = 0usize;
    let mut paren_depth = 0usize;
    let mut bracket_depth = 0usize;
    let mut quote = None;
    let mut malformed = false;

    let push = |compound_start: usize,
                end: usize,
                reaches: InvalidationReaches,
                compounds: &mut Vec<(String, InvalidationReaches)>| {
        let compound: String = chars[compound_start..end].iter().collect();
        let compound = compound.trim();
        if !compound.is_empty() {
            compounds.push((compound.to_string(), reaches));
            true
        } else {
            false
        }
    };

    while index < chars.len() {
        let ch = chars[index];
        if ch == '\\' {
            index = (index + 2).min(chars.len());
            continue;
        }
        if let Some(active_quote) = quote {
            if ch == active_quote {
                quote = None;
            }
            index += 1;
            continue;
        }
        if ch == '\'' || ch == '"' {
            quote = Some(ch);
            index += 1;
            continue;
        }
        match ch {
            '(' => paren_depth += 1,
            ')' => {
                if paren_depth == 0 {
                    malformed = true;
                } else {
                    paren_depth -= 1;
                }
            }
            '[' => bracket_depth += 1,
            ']' => {
                if bracket_depth == 0 {
                    malformed = true;
                } else {
                    bracket_depth -= 1;
                }
            }
            '>' | '+' | '~' if paren_depth == 0 && bracket_depth == 0 => {
                let reaches = if ch == '>' {
                    InvalidationReaches::DESCENDANTS
                } else {
                    InvalidationReaches::SIBLINGS
                };
                if !push(start, index, reaches, &mut compounds) {
                    malformed = true;
                }
                index += 1;
                while chars.get(index).is_some_and(|ch| ch.is_whitespace()) {
                    index += 1;
                }
                start = index;
                continue;
            }
            ',' if paren_depth == 0 && bracket_depth == 0 => malformed = true,
            _ if ch.is_whitespace() && paren_depth == 0 && bracket_depth == 0 => {
                let whitespace = index;
                while chars.get(index).is_some_and(|ch| ch.is_whitespace()) {
                    index += 1;
                }
                if matches!(chars.get(index), Some('>') | Some('+') | Some('~')) {
                    continue;
                }
                if index < chars.len() {
                    if !push(
                        start,
                        whitespace,
                        InvalidationReaches::DESCENDANTS,
                        &mut compounds,
                    ) {
                        malformed = true;
                    }
                    start = index;
                    continue;
                }
                break;
            }
            _ => {}
        }
        index += 1;
    }
    if paren_depth != 0 || bracket_depth != 0 || quote.is_some() {
        malformed = true;
    }
    let tail: String = chars[start..].iter().collect();
    let tail = tail.trim();
    if !tail.is_empty() {
        compounds.push((tail.to_string(), InvalidationReaches::SELF));
    } else if compounds.is_empty() {
        malformed = true;
    }
    let mut descendants_to_right = false;
    for (_, reaches) in compounds.iter_mut().rev() {
        if reaches.contains(InvalidationReaches::DESCENDANTS) {
            descendants_to_right = true;
        } else if descendants_to_right && reaches.contains(InvalidationReaches::SIBLINGS) {
            // `.foo ~ .bar .child`: a change to `.foo` can affect descendants
            // of following siblings. A flat Siblings reach is insufficient
            // unless phase 2 also carries the remaining descendant path.
            *reaches = reaches.union(InvalidationReaches::CONSERVATIVE);
        }
    }
    (compounds, malformed)
}

fn nth_of_selector(arguments: &str) -> Option<&str> {
    let chars: Vec<char> = arguments.chars().collect();
    let mut bracket_depth = 0usize;
    let mut paren_depth = 0usize;
    let mut quote = None;
    let mut index = 0usize;
    while index + 1 < chars.len() {
        let ch = chars[index];
        if ch == '\\' {
            index += 2;
            continue;
        }
        if let Some(active_quote) = quote {
            if ch == active_quote {
                quote = None;
            }
            index += 1;
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '[' => bracket_depth += 1,
            ']' => bracket_depth = bracket_depth.saturating_sub(1),
            '(' => paren_depth += 1,
            ')' => paren_depth = paren_depth.saturating_sub(1),
            _ => {}
        }
        if bracket_depth == 0
            && paren_depth == 0
            && (index == 0 || chars[index - 1].is_whitespace())
            && ch.eq_ignore_ascii_case(&'o')
            && chars[index + 1].eq_ignore_ascii_case(&'f')
            && chars.get(index + 2).is_none_or(|ch| ch.is_whitespace())
        {
            let byte_index = arguments
                .char_indices()
                .nth(index + 2)
                .map_or(arguments.len(), |(byte, _)| byte);
            return Some(arguments[byte_index..].trim());
        }
        index += 1;
    }
    None
}

fn compound_local_name(compound: &str) -> Option<String> {
    let chars = compound.trim().chars().collect::<Vec<_>>();
    let index;
    if chars.first() == Some(&'|') {
        index = 1;
    } else if chars.first() == Some(&'*') {
        if chars.get(1) != Some(&'|') {
            return None;
        }
        index = 2;
    } else {
        let (first, end) = consume_css_identifier(&chars, 0);
        if first.is_empty() {
            return None;
        }
        if chars.get(end) != Some(&'|') {
            return Some(first.to_ascii_lowercase());
        }
        index = end + 1;
    }
    if chars.get(index) == Some(&'*') {
        return None;
    }
    let (local, _) = consume_css_identifier(&chars, index);
    (!local.is_empty()).then(|| local.to_ascii_lowercase())
}

/// Whether the relative selector's subject compound has a positive key which
/// an inserted/removed subtree can look up. Keys hidden only inside `:is()` or
/// negation are deliberately not credited; treating those as unkeyed costs a
/// broader invalidation but cannot miss an activation.
fn relative_selector_subject_has_key(selector: &str) -> bool {
    let selector = selector.trim();
    let selector = selector
        .chars()
        .next()
        .filter(|character| matches!(character, '>' | '+' | '~'))
        .map(|character| selector[character.len_utf8()..].trim_start())
        .unwrap_or(selector);
    let (compounds, malformed) = invalidation_compounds(selector);
    if malformed {
        return false;
    }
    let Some((subject, _)) = compounds.last() else {
        return false;
    };
    if compound_local_name(subject).is_some() {
        return true;
    }
    let chars = subject.chars().collect::<Vec<_>>();
    let mut paren_depth = 0usize;
    let mut bracket_depth = 0usize;
    let mut quote = None;
    let mut index = 0usize;
    while index < chars.len() {
        let ch = chars[index];
        if ch == '\\' {
            index = (index + 2).min(chars.len());
            continue;
        }
        if let Some(active_quote) = quote {
            if ch == active_quote {
                quote = None;
            }
            index += 1;
            continue;
        }
        if ch == '\'' || ch == '"' {
            quote = Some(ch);
        } else if ch == ':' && paren_depth == 0 && bracket_depth == 0 {
            let (name, next) = consume_css_identifier(&chars, index + 1);
            if matches!(name.to_ascii_lowercase().as_str(), "is" | "where")
                && chars.get(next) == Some(&'(')
            {
                let Some(close) = matching_delimiter(&chars, next, '(', ')') else {
                    return false;
                };
                let arguments = chars[next + 1..close].iter().collect::<String>();
                let alternatives = split_selector_list(&arguments);
                if !alternatives.is_empty()
                    && alternatives.iter().all(|alternative| {
                        relative_selector_subject_has_key(alternative.trim())
                    })
                {
                    return true;
                }
                index = close + 1;
                continue;
            }
        } else if ch == '(' {
            paren_depth += 1;
        } else if ch == ')' {
            paren_depth = paren_depth.saturating_sub(1);
        } else if ch == '[' && paren_depth == 0 {
            if bracket_depth == 0 {
                return true;
            }
            bracket_depth += 1;
        } else if ch == ']' && paren_depth == 0 {
            bracket_depth = bracket_depth.saturating_sub(1);
        } else if paren_depth == 0 && bracket_depth == 0 && matches!(ch, '#' | '.') {
            return true;
        }
        index += 1;
    }
    false
}

/// Pick one positive key which every match of this exact anchor compound must
/// have. Functional pseudos are skipped as opaque boolean expressions; using
/// a key from one `:is()` arm as a mandatory filter would be unsound.
fn relational_anchor_key(compound: &str) -> Option<RelationalSelectorKey> {
    if let Some(local_name) = compound_local_name(compound) {
        return Some(RelationalSelectorKey::LocalName(local_name));
    }
    let chars = compound.chars().collect::<Vec<_>>();
    let mut index = 0usize;
    while index < chars.len() {
        match chars[index] {
            '#' => {
                let (id, next) = consume_css_identifier(&chars, index + 1);
                if !id.is_empty() {
                    return Some(RelationalSelectorKey::Id(id));
                }
                index = next.max(index + 1);
            }
            '.' => {
                let (class, next) = consume_css_identifier(&chars, index + 1);
                if !class.is_empty() {
                    return Some(RelationalSelectorKey::Class(class));
                }
                index = next.max(index + 1);
            }
            '[' => {
                let close = matching_delimiter(&chars, index, '[', ']')?;
                let mut name_index = index + 1;
                while chars.get(name_index).is_some_and(|ch| ch.is_whitespace()) {
                    name_index += 1;
                }
                // `Node::get_attribute` is intentionally the unqualified HTML
                // lookup. A namespace-qualified selector cannot use that as a
                // mandatory anchor filter without risking a false rejection.
                if chars.get(name_index) == Some(&'*') || chars.get(name_index) == Some(&'|') {
                    index = close + 1;
                    continue;
                }
                let (first, first_end) = consume_css_identifier(&chars, name_index);
                if chars.get(first_end) == Some(&'|') {
                    index = close + 1;
                    continue;
                }
                let (attribute, end) = (first, first_end);
                if !attribute.is_empty() && end <= close {
                    return Some(RelationalSelectorKey::Attribute(
                        attribute.to_ascii_lowercase(),
                    ));
                }
                index = close + 1;
            }
            ':' => {
                let pseudo_start = index + usize::from(chars.get(index + 1) == Some(&':')) + 1;
                let (_, next) = consume_css_identifier(&chars, pseudo_start);
                if chars.get(next) == Some(&'(') {
                    let close = matching_delimiter(&chars, next, '(', ')')?;
                    index = close + 1;
                } else {
                    index = next.max(index + 1);
                }
            }
            '\\' => index = (index + 2).min(chars.len()),
            _ => index += 1,
        }
    }
    None
}

fn push_relational_selector_key(
    keys: &mut Vec<RelationalSelectorKey>,
    key: RelationalSelectorKey,
) {
    if !keys.contains(&key) {
        keys.push(key);
    }
}

/// Collect cheap positive keys anywhere along a relative-selector path.  A
/// child-list insertion which contains none of these keys cannot make the path
/// start matching.  This is an early rejection only: each key is treated as an
/// alternative trigger, so compounds requiring several keys remain sound.
fn collect_relational_selector_keys(
    selector: &str,
    keys: &mut Vec<RelationalSelectorKey>,
) {
    let selector = selector.trim();
    let selector = selector
        .chars()
        .next()
        .filter(|character| matches!(character, '>' | '+' | '~'))
        .map(|character| selector[character.len_utf8()..].trim_start())
        .unwrap_or(selector);
    let (compounds, _) = invalidation_compounds(selector);
    for (compound, _) in compounds {
        collect_relational_compound_keys(&compound, keys);
    }
}

fn collect_relational_compound_keys(
    compound: &str,
    keys: &mut Vec<RelationalSelectorKey>,
) {
    if let Some(local_name) = compound_local_name(compound) {
        push_relational_selector_key(keys, RelationalSelectorKey::LocalName(local_name));
    }
    let chars = compound.chars().collect::<Vec<_>>();
    let mut index = 0usize;
    while index < chars.len() {
        match chars[index] {
            '#' => {
                let (id, next) = consume_css_identifier(&chars, index + 1);
                if !id.is_empty() {
                    push_relational_selector_key(keys, RelationalSelectorKey::Id(id));
                }
                index = next.max(index + 1);
            }
            '.' => {
                let (class, next) = consume_css_identifier(&chars, index + 1);
                if !class.is_empty() {
                    push_relational_selector_key(keys, RelationalSelectorKey::Class(class));
                }
                index = next.max(index + 1);
            }
            '[' => {
                let Some(close) = matching_delimiter(&chars, index, '[', ']') else {
                    return;
                };
                let mut name_index = index + 1;
                while chars.get(name_index).is_some_and(|ch| ch.is_whitespace()) {
                    name_index += 1;
                }
                let (first, first_end) = if chars.get(name_index) == Some(&'*') {
                    (String::new(), name_index + 1)
                } else {
                    consume_css_identifier(&chars, name_index)
                };
                let (attribute, end) = if chars.get(first_end) == Some(&'|') {
                    consume_css_identifier(&chars, first_end + 1)
                } else {
                    (first, first_end)
                };
                if !attribute.is_empty() && end <= close {
                    push_relational_selector_key(
                        keys,
                        RelationalSelectorKey::Attribute(attribute.to_ascii_lowercase()),
                    );
                }
                index = close + 1;
            }
            ':' => {
                let pseudo_start = index + usize::from(chars.get(index + 1) == Some(&':')) + 1;
                let (name, next) = consume_css_identifier(&chars, pseudo_start);
                if chars.get(next) != Some(&'(') {
                    index = next.max(index + 1);
                    continue;
                }
                let Some(close) = matching_delimiter(&chars, next, '(', ')') else {
                    return;
                };
                let arguments = chars[next + 1..close].iter().collect::<String>();
                match name.to_ascii_lowercase().as_str() {
                    "is" | "where" | "not" | "has" => {
                        for alternative in split_selector_list(&arguments) {
                            collect_relational_selector_keys(alternative.trim(), keys);
                        }
                    }
                    "nth-child" | "nth-last-child" | "nth-of-type" | "nth-last-of-type" => {
                        if let Some(of_selector) = nth_of_selector(&arguments) {
                            for alternative in split_selector_list(of_selector) {
                                collect_relational_selector_keys(alternative.trim(), keys);
                            }
                        }
                    }
                    _ => {}
                }
                index = close + 1;
            }
            '\\' => index = (index + 2).min(chars.len()),
            _ => index += 1,
        }
    }
}

fn selector_contains_pseudo(selector: &str, names: &[&str]) -> bool {
    let chars = selector.chars().collect::<Vec<_>>();
    let mut index = 0usize;
    let mut quote = None;
    let mut bracket_depth = 0usize;
    while index < chars.len() {
        let ch = chars[index];
        if ch == '\\' {
            index = (index + 2).min(chars.len());
            continue;
        }
        if let Some(active_quote) = quote {
            if ch == active_quote {
                quote = None;
            }
            index += 1;
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '[' => bracket_depth += 1,
            ']' => bracket_depth = bracket_depth.saturating_sub(1),
            ':' if bracket_depth == 0 && chars.get(index + 1) != Some(&':') => {
                let (name, next) = consume_css_identifier(&chars, index + 1);
                if names
                    .iter()
                    .any(|expected| name.eq_ignore_ascii_case(expected))
                {
                    return true;
                }
                index = next.max(index + 1);
                continue;
            }
            _ => {}
        }
        index += 1;
    }
    false
}

fn selector_contains_adjacent_combinator(selector: &str) -> bool {
    let chars = selector.chars().collect::<Vec<_>>();
    let mut index = 0usize;
    let mut quote = None;
    let mut bracket_depth = 0usize;
    while index < chars.len() {
        let ch = chars[index];
        if ch == '\\' {
            index = (index + 2).min(chars.len());
            continue;
        }
        if let Some(active_quote) = quote {
            if ch == active_quote {
                quote = None;
            }
            index += 1;
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '[' => bracket_depth += 1,
            ']' => bracket_depth = bracket_depth.saturating_sub(1),
            '+' if bracket_depth == 0 => return true,
            _ => {}
        }
        index += 1;
    }
    false
}

/// Sibling combinators which participate in the current selector path.
///
/// Parentheses are deliberately skipped here. `:is()`/`:where()`/`:not()`
/// alternatives are fed back through dependency collection separately, while
/// `:has()` owns an upward invalidation path and must not poison ordinary
/// sibling invalidation outside its anchor.
fn selector_sibling_combinators(selector: &str) -> (bool, bool) {
    let chars = selector.chars().collect::<Vec<_>>();
    let mut index = 0usize;
    let mut quote = None;
    let mut bracket_depth = 0usize;
    let mut paren_depth = 0usize;
    let mut adjacent = false;
    let mut general = false;
    while index < chars.len() {
        let ch = chars[index];
        if ch == '\\' {
            index = (index + 2).min(chars.len());
            continue;
        }
        if let Some(active_quote) = quote {
            if ch == active_quote {
                quote = None;
            }
            index += 1;
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '[' => bracket_depth += 1,
            ']' => bracket_depth = bracket_depth.saturating_sub(1),
            '(' if bracket_depth == 0 => paren_depth += 1,
            ')' if bracket_depth == 0 => paren_depth = paren_depth.saturating_sub(1),
            '+' if bracket_depth == 0 && paren_depth == 0 => adjacent = true,
            '~' if bracket_depth == 0 && paren_depth == 0 => general = true,
            _ => {}
        }
        index += 1;
    }
    (adjacent, general)
}

fn note_compound_dependencies(
    map: &mut InvalidationMap,
    compound: &str,
    reaches: InvalidationReaches,
    rule_order: usize,
    inside_relational: bool,
) {
    if let Some(local_name) = compound_local_name(compound) {
        map.push_local_name(local_name, rule_order, reaches);
    }
    let chars: Vec<char> = compound.chars().collect();
    let mut index = 0usize;
    while index < chars.len() {
        match chars[index] {
            '#' => {
                let (id, next) = consume_css_identifier(&chars, index + 1);
                if id.is_empty() {
                    map.mark_conservative(rule_order);
                } else {
                    map.push_id(id, rule_order, reaches);
                }
                index = next.max(index + 1);
            }
            '.' => {
                let (class, next) = consume_css_identifier(&chars, index + 1);
                if class.is_empty() {
                    map.mark_conservative(rule_order);
                } else {
                    map.push_class(class, rule_order, reaches);
                }
                index = next.max(index + 1);
            }
            '[' => {
                let Some(close) = matching_delimiter(&chars, index, '[', ']') else {
                    map.mark_conservative(rule_order);
                    return;
                };
                let mut name_index = index + 1;
                while chars.get(name_index).is_some_and(|ch| ch.is_whitespace()) {
                    name_index += 1;
                }
                if chars.get(name_index) == Some(&'*') || chars.get(name_index) == Some(&'|') {
                    name_index += 1;
                }
                let (first, first_end) = consume_css_identifier(&chars, name_index);
                let (attribute, end) = if chars.get(first_end) == Some(&'|') {
                    consume_css_identifier(&chars, first_end + 1)
                } else {
                    (first, first_end)
                };
                if attribute.is_empty() || end > close {
                    map.mark_conservative(rule_order);
                } else {
                    map.push_attribute(attribute, rule_order, reaches);
                }
                index = close + 1;
            }
            ':' => {
                if chars.get(index + 1) == Some(&':') {
                    let (_, next) = consume_css_identifier(&chars, index + 2);
                    index = next.max(index + 2);
                    continue;
                }
                let (name, next) = consume_css_identifier(&chars, index + 1);
                let name = name.to_ascii_lowercase();
                if name.is_empty() {
                    map.mark_conservative(rule_order);
                    index += 1;
                    continue;
                }
                if chars.get(next) != Some(&'(') {
                    map.push_state(name.clone(), rule_order, reaches);
                    if matches!(
                        name.as_str(),
                        "empty"
                            | "first-child"
                            | "last-child"
                            | "only-child"
                            | "first-of-type"
                            | "last-of-type"
                            | "only-of-type"
                    ) {
                        map.push_structural_invalidation(StructuralInvalidation {
                            rule_order,
                            state: name.clone(),
                            subject_key: relational_anchor_key(compound),
                            reaches,
                            inside_relational,
                        });
                    }
                    if matches!(
                        name.as_str(),
                        "root"
                            | "scope"
                            | "target"
                            | "link"
                            | "any-link"
                            | "visited"
                            | "empty"
                            | "first-child"
                            | "last-child"
                            | "only-child"
                            | "first-of-type"
                            | "last-of-type"
                            | "only-of-type"
                    ) {
                        // These change when nodes are inserted, removed, or
                        // reordered. Phase 2 has no tree-structural mutation
                        // lookup yet, so keep the complete cascade fallback.
                        map.mark_conservative(rule_order);
                    }
                    index = next;
                    continue;
                }
                let Some(close) = matching_delimiter(&chars, next, '(', ')') else {
                    map.mark_conservative(rule_order);
                    return;
                };
                let arguments: String = chars[next + 1..close].iter().collect();
                match name.as_str() {
                    "is" | "where" | "not" => {
                        for alternative in split_selector_list(&arguments) {
                            note_selector_dependencies(
                                map,
                                alternative.trim(),
                                reaches,
                                rule_order,
                                !inside_relational,
                            );
                        }
                    }
                    "has" => {
                        // Relative selectors invalidate anchors upwards, which
                        // Self/Descendants/Siblings cannot express soundly.
                        map.push_state(
                            name.clone(),
                            rule_order,
                            InvalidationReaches::CONSERVATIVE,
                        );
                        map.mark_conservative(rule_order);
                        let alternatives = split_selector_list(&arguments);
                        let mut relative_keys = Vec::new();
                        for alternative in &alternatives {
                            collect_relational_selector_keys(
                                alternative.trim(),
                                &mut relative_keys,
                            );
                        }
                        let unkeyed = alternatives.iter().any(|alternative| {
                            !relative_selector_subject_has_key(alternative.trim())
                        });
                        map.mark_relational(rule_order, unkeyed);
                        let sibling_side_effect = alternatives
                            .iter()
                            .any(|alternative| {
                                selector_contains_adjacent_combinator(alternative.trim())
                            });
                        let structural_side_effect = alternatives.iter().any(|alternative| {
                            selector_contains_pseudo(
                                alternative.trim(),
                                &[
                                    "empty",
                                    "first-child",
                                    "last-child",
                                    "only-child",
                                    "first-of-type",
                                    "last-of-type",
                                    "only-of-type",
                                    "nth-child",
                                    "nth-last-child",
                                    "nth-of-type",
                                    "nth-last-of-type",
                                ],
                            )
                        });
                        let text_side_effect = alternatives.iter().any(|alternative| {
                            selector_contains_pseudo(alternative.trim(), &["empty"])
                        });
                        map.push_relational_invalidation(RelationalInvalidation {
                            rule_order,
                            anchor_key: relational_anchor_key(compound),
                            relative_keys,
                            anchor_reaches: reaches,
                            unkeyed_subject: unkeyed,
                            sibling_side_effect,
                            structural_side_effect,
                            text_side_effect,
                            unrepresentable_outer_path: reaches
                                .contains(InvalidationReaches::CONSERVATIVE),
                        });
                        for alternative in alternatives {
                            note_selector_dependencies(
                                map,
                                alternative.trim(),
                                InvalidationReaches::CONSERVATIVE,
                                rule_order,
                                false,
                            );
                        }
                    }
                    "nth-child" | "nth-last-child" | "nth-of-type" | "nth-last-of-type" => {
                        map.push_state(name.clone(), rule_order, reaches);
                        map.push_structural_invalidation(StructuralInvalidation {
                            rule_order,
                            state: name,
                            subject_key: relational_anchor_key(compound),
                            reaches,
                            inside_relational,
                        });
                        // Structural index changes and `of <complex-selector>`
                        // need sibling-wide bookkeeping not present in phase 1.
                        map.mark_conservative(rule_order);
                        if let Some(of_selector) = nth_of_selector(&arguments) {
                            for alternative in split_selector_list(of_selector) {
                                note_selector_dependencies(
                                    map,
                                    alternative.trim(),
                                    InvalidationReaches::CONSERVATIVE,
                                    rule_order,
                                    !inside_relational,
                                );
                            }
                        }
                    }
                    "dir" | "lang" => {
                        // The corresponding HTML attributes inherit through
                        // descendants, while the functional selector may
                        // observe language/direction resolved above the
                        // mutated node. Keep the dependency keyed, but require
                        // the retained planner's sound full fallback.
                        map.push_state(
                            name,
                            rule_order,
                            InvalidationReaches::CONSERVATIVE,
                        );
                        map.mark_conservative(rule_order);
                    }
                    _ => {
                        // A compiled functional pseudo outside the explicitly
                        // modeled set may hide selector or document state.
                        map.push_state(name, rule_order, reaches);
                        map.mark_conservative(rule_order);
                    }
                }
                index = close + 1;
            }
            '\\' => index = (index + 2).min(chars.len()),
            _ => index += 1,
        }
    }
}

fn note_selector_dependencies(
    map: &mut InvalidationMap,
    selector: &str,
    outer_reaches: InvalidationReaches,
    rule_order: usize,
    record_tree_siblings: bool,
) {
    if record_tree_siblings {
        let (adjacent, general) = selector_sibling_combinators(selector);
        map.adjacent_sibling_selectors |= adjacent;
        map.general_sibling_selectors |= general;
    }
    let (compounds, malformed) = invalidation_compounds(selector);
    if malformed {
        map.mark_conservative(rule_order);
    }
    for (compound, local_reaches) in compounds {
        if record_tree_siblings
            && local_reaches.contains(InvalidationReaches::SIBLINGS)
            && !relative_selector_subject_has_key(&compound)
        {
            map.unkeyed_sibling_selectors = true;
        }
        if local_reaches.contains(InvalidationReaches::CONSERVATIVE) {
            map.mark_conservative(rule_order);
        }
        let reaches = compose_invalidation_reach(map, local_reaches, outer_reaches, rule_order);
        note_compound_dependencies(
            map,
            &compound,
            reaches,
            rule_order,
            !record_tree_siblings,
        );
    }
}

fn note_selector_for_invalidation(
    map: &mut InvalidationMap,
    selector: &str,
    rule_order: usize,
) {
    note_selector_dependencies(
        map,
        selector,
        InvalidationReaches::SELF,
        rule_order,
        true,
    );
}

/// Record element attributes read from declaration values.
///
/// Selector invalidation alone is insufficient for generated content such as
/// `.label::before { content: attr(data-label) }`: changing `data-label`
/// changes the computed pseudo style even though the attribute does not occur
/// in the selector.  Keep this deliberately broader than the property parser;
/// an extra self invalidation is cheap, while missing a supported `attr()`
/// spelling would retain stale computed values.
fn note_declaration_attribute_dependencies(
    map: &mut InvalidationMap,
    declarations: &str,
    rule_order: usize,
) {
    // Avoid another declaration-vector allocation on the stylesheet hot path:
    // almost every rule exits through this byte scan, and the uncommon rule
    // containing `attr` pays for the balanced character walk below.
    if !declarations
        .as_bytes()
        .windows(4)
        .any(|window| window.eq_ignore_ascii_case(b"attr"))
    {
        return;
    }
    let chars: Vec<char> = declarations.chars().collect();
    let mut index = 0usize;
    let mut quote = None;
    while index < chars.len() {
        let ch = chars[index];
        if ch == '\\' {
            index = (index + 2).min(chars.len());
            continue;
        }
        if let Some(active_quote) = quote {
            if ch == active_quote {
                quote = None;
            }
            index += 1;
            continue;
        }
        if ch == '\'' || ch == '"' {
            quote = Some(ch);
            index += 1;
            continue;
        }
        if ch == '/' && chars.get(index + 1) == Some(&'*') {
            index += 2;
            while index + 1 < chars.len()
                && !(chars[index] == '*' && chars[index + 1] == '/')
            {
                index += 1;
            }
            index = (index + 2).min(chars.len());
            continue;
        }
        if !(ch.is_alphabetic() || ch == '_' || ch == '-' || !ch.is_ascii()) {
            index += 1;
            continue;
        }
        let (function, next) = consume_css_identifier(&chars, index);
        index = next.max(index + 1);
        if !function.eq_ignore_ascii_case("attr") {
            continue;
        }
        let mut open = index;
        while chars.get(open).is_some_and(|ch| ch.is_whitespace()) {
            open += 1;
        }
        if chars.get(open) != Some(&'(') {
            continue;
        }
        let Some(close) = matching_delimiter(&chars, open, '(', ')') else {
            // An unterminated function cannot be consumed by the current
            // declaration parser, so it has no live attribute dependency.
            break;
        };
        let mut name_start = open + 1;
        while chars
            .get(name_start)
            .is_some_and(|ch| ch.is_whitespace())
        {
            name_start += 1;
        }
        let (attribute, name_end) = consume_css_identifier(&chars, name_start);
        if !attribute.is_empty() && name_end <= close {
            map.push_attribute(attribute, rule_order, InvalidationReaches::SELF);
        }
        index = close + 1;
    }
}

fn selector_requires_conservative_tracking(selector: &str) -> bool {
    let selector = selector.to_ascii_lowercase();
    selector.contains(":has(")
        || selector.contains(":dir(")
        || selector.contains(":lang(")
        || selector.contains(":nth-child(")
        || selector.contains(":nth-last-child(")
        || selector.contains(":nth-of-type(")
        || selector.contains(":nth-last-of-type(")
        || selector.contains(":target")
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
struct ContainerConditionId(u32);

impl ContainerConditionId {
    const NONE: Self = Self(0);
}

#[derive(Clone, Debug, PartialEq)]
struct ContainerConditionNode {
    parent: ContainerConditionId,
    /// Comma-separated queries in one prelude are alternatives; parent-linked
    /// nodes represent nested `@container` rules that must all match.
    alternatives: Vec<ContainerQuery>,
}

#[derive(Clone, Debug, PartialEq)]
struct ContainerQuery {
    name: Option<String>,
    condition: Option<ContainerQueryExpr>,
}

#[derive(Clone, Debug, PartialEq)]
enum ContainerQueryExpr {
    Feature(ContainerSizeFeature),
    /// Syntactically valid future/general-enclosed syntax has Kleene
    /// `unknown` truth. Retaining it prevents one unknown comma arm from
    /// discarding supported alternatives in the same `@container` rule.
    Unknown,
    Not(Box<ContainerQueryExpr>),
    And(Vec<ContainerQueryExpr>),
    Or(Vec<ContainerQueryExpr>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ContainerQueryAxis {
    Width,
    Height,
    InlineSize,
    BlockSize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ContainerQueryComparison {
    Min,
    Max,
    GreaterThan,
    LessThan,
    Equal,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ContainerSizeFeature {
    axis: ContainerQueryAxis,
    comparison: ContainerQueryComparison,
    length: ContainerQueryLength,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum ContainerQueryLength {
    Px(f32),
    Em(f32),
    Rem(f32),
}

struct ParsedRule {
    selector: String,
    declarations: String,
    container_condition_id: ContainerConditionId,
    layer: Option<LayerOrder>,
}

struct Rule {
    sel: CompiledSelector,
    specificity: u32,
    normal_decls: String,
    important_decls: String,
    normal_flags: DeclarationStreamFlags,
    important_flags: DeclarationStreamFlags,
    candidate_slot: u32,
    /// Source order, for breaking specificity ties (later wins).
    order: usize,
    container_condition_id: ContainerConditionId,
    layer: Option<LayerOrder>,
}

#[derive(Default)]
struct ShadowScopeDeclarations {
    normal: String,
    important: String,
}

pub(crate) struct ShadowSlottedScope<'a> {
    pub sheet: &'a Stylesheet,
    pub host: NodeId,
}

fn append_declaration_stream(target: &mut String, declarations: &str) {
    if declarations.trim().is_empty() {
        return;
    }
    if !target.is_empty() && !target.ends_with(';') {
        target.push(';');
    }
    target.push_str(declarations);
}

const NO_CANDIDATE_SLOT: u32 = u32::MAX;

fn is_root_element(tree: &DomTree, nid: NodeId) -> bool {
    tree.get_node(nid)
        .and_then(|node| node.parent)
        .and_then(|parent| tree.get_node(parent))
        .is_some_and(|parent| parent.is_document())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct DeclarationStreamFlags {
    has_custom_properties: bool,
    has_var: bool,
    has_color_scheme: bool,
    has_animation: bool,
    has_transform: bool,
    has_opacity: bool,
}

/// Cache the declaration features which otherwise require an extra stream
/// walk or an allocated substituted copy for every matching element. This is
/// computed once while the stylesheet is indexed; false positives would only
/// cost work, while exact declaration-name checks keep the skip paths sound.
fn declaration_stream_flags(css: &str) -> DeclarationStreamFlags {
    let mut flags = DeclarationStreamFlags::default();
    for declaration in crate::style::split_declarations(css) {
        let Some((name, value)) = declaration.split_once(':') else {
            continue;
        };
        let name = name.trim();
        flags.has_custom_properties |= name.starts_with("--") && name.len() > 2;
        flags.has_var |= value.contains("var(");
        if name.eq_ignore_ascii_case("color-scheme") {
            flags.has_color_scheme = true;
        }
        if name.eq_ignore_ascii_case("animation")
            || name
                .get(..10)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("animation-"))
        {
            flags.has_animation = true;
        }
        if name.eq_ignore_ascii_case("transform") || name.eq_ignore_ascii_case("all") {
            flags.has_transform = true;
        }
        if name.eq_ignore_ascii_case("opacity") || name.eq_ignore_ascii_case("all") {
            flags.has_opacity = true;
        }
    }
    flags
}

struct PseudoRule {
    sel: CompiledSelector,
    specificity: u32,
    normal_decls: String,
    important_decls: String,
    normal_flags: DeclarationStreamFlags,
    important_flags: DeclarationStreamFlags,
    candidate_slot: u32,
    order: usize,
    container_condition_id: ContainerConditionId,
    layer: Option<LayerOrder>,
}

#[derive(Default)]
struct PseudoRuleMap {
    rules: Vec<PseudoRule>,
    by_root: Vec<usize>,
    by_id: HashMap<String, Vec<usize>>,
    by_class: HashMap<String, Vec<usize>>,
    by_attribute: HashMap<String, Vec<usize>>,
    by_local: HashMap<String, Vec<usize>>,
    universal: Vec<usize>,
    candidate_slot_count: usize,
}

impl PseudoRuleMap {
    fn push(&mut self, mut rule: PseudoRule) {
        let index = self.rules.len();
        if rule.sel.candidate_keys().len() > 1 {
            rule.candidate_slot = u32::try_from(self.candidate_slot_count)
                .expect("pseudo selector candidate slot count exceeds u32");
            self.candidate_slot_count += 1;
        }
        for key in rule.sel.candidate_keys() {
            match key {
                SelectorKey::Root => self.by_root.push(index),
                SelectorKey::Id(value) => self.by_id.entry(value.clone()).or_default().push(index),
                SelectorKey::Class(value) => {
                    self.by_class.entry(value.clone()).or_default().push(index)
                }
                SelectorKey::Attribute(value) => self
                    .by_attribute
                    .entry(value.clone())
                    .or_default()
                    .push(index),
                SelectorKey::Local(value) => {
                    self.by_local.entry(value.clone()).or_default().push(index)
                }
                SelectorKey::Universal => self.universal.push(index),
            }
        }
        self.rules.push(rule);
    }

    fn bucket_matches_container_query_rule(
        &self,
        bucket: Option<&Vec<usize>>,
        tree: &DomTree,
        matcher: &mut Matcher,
        nid: NodeId,
    ) -> bool {
        bucket.is_some_and(|indices| {
            indices.iter().copied().any(|index| {
                let rule = &self.rules[index];
                rule.container_condition_id != ContainerConditionId::NONE
                    && (rule.candidate_slot == NO_CANDIDATE_SLOT
                        || matcher.mark_candidate(rule.candidate_slot as usize))
                    && matcher.matches(tree, nid, &rule.sel)
            })
        })
    }

    fn node_matches_container_query_rule(
        &self,
        tree: &DomTree,
        matcher: &mut Matcher,
        nid: NodeId,
    ) -> bool {
        if self.candidate_slot_count != 0 {
            matcher.begin_candidate_collection(self.candidate_slot_count);
        }
        let Some(node) = tree.get_node(nid) else {
            return false;
        };
        node.as_element().is_some_and(|element| {
            self.bucket_matches_container_query_rule(
                self.by_local.get(element.local.as_ref()),
                tree,
                matcher,
                nid,
            ) || (is_root_element(tree, nid)
                && self.bucket_matches_container_query_rule(
                    (!self.by_root.is_empty()).then_some(&self.by_root),
                    tree,
                    matcher,
                    nid,
                ))
                || node.get_attribute("id").is_some_and(|id| {
                    self.bucket_matches_container_query_rule(
                        self.by_id.get(id),
                        tree,
                        matcher,
                        nid,
                    )
                })
                || node.get_attribute("class").is_some_and(|classes| {
                    classes.split_whitespace().any(|class| {
                        self.bucket_matches_container_query_rule(
                            self.by_class.get(class),
                            tree,
                            matcher,
                            nid,
                        )
                    })
                })
                || node.attrs().is_some_and(|attributes| {
                    attributes.iter().any(|attribute| {
                        self.bucket_matches_container_query_rule(
                            self.by_attribute.get(attribute.name.local.as_ref()),
                            tree,
                            matcher,
                            nid,
                        )
                    })
                })
                || self.bucket_matches_container_query_rule(
                    (!self.universal.is_empty()).then_some(&self.universal),
                    tree,
                    matcher,
                    nid,
                )
        })
    }
}

/// A cascade layer's first-declaration position at each nesting level.
///
/// The terminal (direct declarations in a parent layer) is intentionally not
/// stored. [`compare_layer_order`] treats a shorter prefix as the implicit
/// final sub-layer, matching CSS Cascade 5: normal declarations directly in a
/// layer outrank its nested layers, and the order reverses for `!important`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct LayerOrder(Vec<u32>);

/// Global author-layer name table for one `Stylesheet` parse. Layer ordering
/// spans all style sources in document order, so this registry must outlive
/// each individual `<style>`/linked stylesheet source.
#[derive(Default)]
struct LayerRegistry {
    named: HashMap<(Vec<u32>, String), LayerOrder>,
    next_child: HashMap<Vec<u32>, u32>,
}

impl LayerRegistry {
    fn allocate(&mut self, parent: &[u32]) -> LayerOrder {
        let next = self.next_child.entry(parent.to_vec()).or_default();
        let order = *next;
        *next = (*next).saturating_add(1);
        let mut path = parent.to_vec();
        path.push(order);
        LayerOrder(path)
    }

    fn register_named(
        &mut self,
        parent: Option<&LayerOrder>,
        qualified_name: &str,
    ) -> Option<LayerOrder> {
        let mut path = parent.map_or_else(Vec::new, |layer| layer.0.clone());
        for component in qualified_name.split('.') {
            let component = component.trim();
            if component.is_empty() {
                return None;
            }
            let key = (path.clone(), component.to_string());
            let layer = if let Some(layer) = self.named.get(&key) {
                layer.clone()
            } else {
                let layer = self.allocate(&path);
                self.named.insert(key, layer.clone());
                layer
            };
            path = layer.0.clone();
        }
        Some(LayerOrder(path))
    }

    fn register_anonymous(&mut self, parent: Option<&LayerOrder>) -> LayerOrder {
        self.allocate(parent.map(|layer| layer.0.as_slice()).unwrap_or_default())
    }

    fn register_statement(&mut self, parent: Option<&LayerOrder>, prelude: &str) {
        for name in prelude.split(',') {
            let _ = self.register_named(parent, name.trim());
        }
    }
}

/// Compare author layer precedence from weak to strong for normal declarations.
/// Unlayered declarations beat every layered declaration. Within layers, later
/// siblings win; declarations directly in a parent layer are its implicit last
/// sub-layer. `!important` uses the exact reverse order.
fn compare_layer_order(
    left: Option<&LayerOrder>,
    right: Option<&LayerOrder>,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (left, right) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(left), Some(right)) => {
            for (a, b) in left.0.iter().zip(&right.0) {
                match a.cmp(b) {
                    Ordering::Equal => {}
                    ordering => return ordering,
                }
            }
            match left.0.len().cmp(&right.0.len()) {
                // A direct declaration in the containing layer is the
                // implicit final sub-layer, not an early prefix.
                Ordering::Less => Ordering::Greater,
                Ordering::Greater => Ordering::Less,
                Ordering::Equal => Ordering::Equal,
            }
        }
    }
}

fn compare_rule_cascade(
    left_layer: Option<&LayerOrder>,
    left_specificity: u32,
    left_order: usize,
    right_layer: Option<&LayerOrder>,
    right_specificity: u32,
    right_order: usize,
    important: bool,
) -> std::cmp::Ordering {
    let layer = compare_layer_order(left_layer, right_layer);
    let layer = if important { layer.reverse() } else { layer };
    layer
        .then(left_specificity.cmp(&right_specificity))
        .then(left_order.cmp(&right_order))
}

const PROPERTY_REGISTRATION_SELECTOR_PREFIX: &str = "\0property:";
const KEYFRAMES_SELECTOR_PREFIX: &str = "\0keyframes:";
const WEBKIT_KEYFRAMES_SELECTOR_PREFIX: &str = "\0webkit-keyframes:";

#[derive(Clone, Debug, PartialEq, Eq)]
struct RegisteredCustomProperty {
    syntax: String,
    inherits: bool,
    initial_value: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
struct KeyframeStop {
    /// CSS keyframes always provide an offset. Keeping this optional makes the
    /// normalization rule explicit and reusable by future script-created
    /// keyframes without changing the sampler.
    offset: Option<f32>,
    declarations: String,
    source_order: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum AnimatedProperty {
    Transform,
    Translate,
    Rotate,
    Scale,
    Width,
    Height,
    MinWidth,
    MinHeight,
    MaxWidth,
    MaxHeight,
    Top,
    Right,
    Bottom,
    Left,
    MarginTop,
    MarginRight,
    MarginBottom,
    MarginLeft,
    PaddingTop,
    PaddingRight,
    PaddingBottom,
    PaddingLeft,
    RowGap,
    ColumnGap,
    FlexBasis,
    Opacity,
    Color,
    BackgroundColor,
    BorderTopColor,
    BorderRightColor,
    BorderBottomColor,
    BorderLeftColor,
    BackgroundPosition,
    Visibility,
}

impl AnimatedProperty {
    fn effect_impact(self) -> crate::AnimationEffectImpact {
        use AnimatedProperty::*;
        match self {
            Opacity
            | Color
            | BackgroundColor
            | BorderTopColor
            | BorderRightColor
            | BorderBottomColor
            | BorderLeftColor
            | BackgroundPosition
            | Visibility => crate::AnimationEffectImpact::Paint,
            Transform
            | Translate
            | Rotate
            | Scale
            | Width
            | Height
            | MinWidth
            | MinHeight
            | MaxWidth
            | MaxHeight
            | Top
            | Right
            | Bottom
            | Left
            | MarginTop
            | MarginRight
            | MarginBottom
            | MarginLeft
            | PaddingTop
            | PaddingRight
            | PaddingBottom
            | PaddingLeft
            | RowGap
            | ColumnGap
            | FlexBasis => crate::AnimationEffectImpact::Geometry,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AnimatedDeclaration {
    name: String,
    value: String,
}

#[derive(Clone, Debug, PartialEq)]
struct PropertyTrackStop {
    offset: f32,
    source_order: usize,
    declaration: AnimatedDeclaration,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Keyframes {
    stops: Vec<KeyframeStop>,
    tracks: HashMap<AnimatedProperty, Vec<PropertyTrackStop>>,
}

#[derive(Clone, Debug)]
pub(crate) struct ContainerBox {
    pub(crate) container_type: crate::ContainerType,
    /// Axes on which size containment actually applies to the generated box.
    /// This can be `Normal` even when computed `container-type` is non-normal
    /// (for example a non-atomic inline or an internal table box).
    pub(crate) available_type: crate::ContainerType,
    pub(crate) names: Vec<String>,
    pub(crate) content_width: f32,
    pub(crate) content_height: f32,
    pub(crate) font_size: f32,
}

impl PartialEq for ContainerBox {
    fn eq(&self, other: &Self) -> bool {
        if self.container_type != other.container_type
            || self.available_type != other.available_type
            || self.names != other.names
        {
            return false;
        }
        match self.available_type {
            crate::ContainerType::Normal => true,
            crate::ContainerType::InlineSize => {
                self.content_width == other.content_width && self.font_size == other.font_size
            }
            crate::ContainerType::Size => {
                self.content_width == other.content_width
                    && self.content_height == other.content_height
                    && self.font_size == other.font_size
            }
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ContainerSnapshot {
    pub(crate) boxes: HashMap<NodeId, ContainerBox>,
    pub(crate) root_font_size: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ContainerQueryTruth {
    True,
    False,
    Unknown,
}

impl ContainerQueryTruth {
    fn and(self, other: Self) -> Self {
        match (self, other) {
            (Self::False, _) | (_, Self::False) => Self::False,
            (Self::True, Self::True) => Self::True,
            _ => Self::Unknown,
        }
    }

    fn or(self, other: Self) -> Self {
        match (self, other) {
            (Self::True, _) | (_, Self::True) => Self::True,
            (Self::False, Self::False) => Self::False,
            _ => Self::Unknown,
        }
    }

    fn not(self) -> Self {
        match self {
            Self::True => Self::False,
            Self::False => Self::True,
            Self::Unknown => Self::Unknown,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum ContainerQuerySubjectKind {
    Element,
    OriginatingPseudo,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct ContainerQueryCacheKey {
    subject: NodeId,
    condition: ContainerConditionId,
    kind: ContainerQuerySubjectKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ContainerQueryDecision {
    truth: ContainerQueryTruth,
    selected_containers: Vec<Option<NodeId>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ContainerDecisionSignature {
    decisions: HashMap<ContainerQueryCacheKey, ContainerQueryDecision>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ContainerQueryStats {
    pub(crate) evaluations: usize,
    pub(crate) cache_hits: usize,
    pub(crate) ancestor_steps: usize,
}

pub(crate) struct ContainerQueryEvaluator<'a> {
    tree: &'a DomTree,
    snapshot: &'a ContainerSnapshot,
    cache: HashMap<ContainerQueryCacheKey, ContainerQueryDecision>,
    stats: ContainerQueryStats,
}

impl<'a> ContainerQueryEvaluator<'a> {
    pub(crate) fn new(tree: &'a DomTree, snapshot: &'a ContainerSnapshot) -> Self {
        Self {
            tree,
            snapshot,
            cache: HashMap::new(),
            stats: ContainerQueryStats::default(),
        }
    }

    pub(crate) fn finish(self) -> (ContainerDecisionSignature, ContainerQueryStats) {
        (
            ContainerDecisionSignature {
                decisions: self.cache,
            },
            self.stats,
        )
    }

    fn condition_matches(
        &mut self,
        sheet: &Stylesheet,
        subject: NodeId,
        condition: ContainerConditionId,
        kind: ContainerQuerySubjectKind,
    ) -> bool {
        self.evaluate_condition_chain(sheet, subject, condition, kind)
            .truth
            == ContainerQueryTruth::True
    }

    fn evaluate_condition_chain(
        &mut self,
        sheet: &Stylesheet,
        subject: NodeId,
        condition: ContainerConditionId,
        kind: ContainerQuerySubjectKind,
    ) -> ContainerQueryDecision {
        if condition == ContainerConditionId::NONE {
            return ContainerQueryDecision {
                truth: ContainerQueryTruth::True,
                selected_containers: Vec::new(),
            };
        }
        let key = ContainerQueryCacheKey {
            subject,
            condition,
            kind,
        };
        if let Some(decision) = self.cache.get(&key) {
            self.stats.cache_hits += 1;
            return decision.clone();
        }
        self.stats.evaluations += 1;
        let node = &sheet.container_conditions[condition.0 as usize];
        let mut own_truth = ContainerQueryTruth::False;
        let mut selected_containers = Vec::with_capacity(node.alternatives.len());
        for query in &node.alternatives {
            let (truth, selected) = self.evaluate_query(subject, kind, query);
            own_truth = own_truth.or(truth);
            selected_containers.push(selected);
            if own_truth == ContainerQueryTruth::True {
                break;
            }
        }
        if own_truth == ContainerQueryTruth::False {
            let decision = ContainerQueryDecision {
                truth: ContainerQueryTruth::False,
                selected_containers,
            };
            self.cache.insert(key, decision.clone());
            return decision;
        }
        let parent = self.evaluate_condition_chain(sheet, subject, node.parent, kind);
        selected_containers.extend(parent.selected_containers);
        let decision = ContainerQueryDecision {
            truth: own_truth.and(parent.truth),
            selected_containers,
        };
        self.cache.insert(key, decision.clone());
        decision
    }

    fn evaluate_query(
        &mut self,
        subject: NodeId,
        kind: ContainerQuerySubjectKind,
        query: &ContainerQuery,
    ) -> (ContainerQueryTruth, Option<NodeId>) {
        let required_axes = query
            .condition
            .as_ref()
            .map(container_query_required_axes)
            .unwrap_or_default();
        let mut candidate = match kind {
            ContainerQuerySubjectKind::Element => {
                self.tree.get_node(subject).and_then(|node| node.parent)
            }
            ContainerQuerySubjectKind::OriginatingPseudo => Some(subject),
        };
        while let Some(id) = candidate {
            self.stats.ancestor_steps += 1;
            let parent = self.tree.get_node(id).and_then(|node| node.parent);
            if let Some(container) = self.snapshot.boxes.get(&id) {
                let supports_axis = match container.container_type {
                    crate::ContainerType::Normal => !required_axes.inline && !required_axes.block,
                    crate::ContainerType::InlineSize => !required_axes.block,
                    crate::ContainerType::Size => true,
                };
                let name_matches = query
                    .name
                    .as_ref()
                    .map_or(true, |name| container.names.iter().any(|item| item == name));
                if supports_axis && name_matches {
                    let axis_available = match container.available_type {
                        crate::ContainerType::Normal => {
                            !required_axes.inline && !required_axes.block
                        }
                        crate::ContainerType::InlineSize => !required_axes.block,
                        crate::ContainerType::Size => true,
                    };
                    if !axis_available {
                        return (ContainerQueryTruth::Unknown, Some(id));
                    }
                    let truth =
                        query
                            .condition
                            .as_ref()
                            .map_or(ContainerQueryTruth::True, |condition| {
                                evaluate_container_query_expr(
                                    condition,
                                    container,
                                    self.snapshot.root_font_size,
                                )
                            });
                    return (truth, Some(id));
                }
            }
            candidate = parent;
        }
        (ContainerQueryTruth::False, None)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ContainerQueryRequiredAxes {
    inline: bool,
    block: bool,
}

fn container_query_required_axes(expr: &ContainerQueryExpr) -> ContainerQueryRequiredAxes {
    match expr {
        ContainerQueryExpr::Feature(feature) => match feature.axis {
            ContainerQueryAxis::Width | ContainerQueryAxis::InlineSize => {
                ContainerQueryRequiredAxes {
                    inline: true,
                    block: false,
                }
            }
            ContainerQueryAxis::Height | ContainerQueryAxis::BlockSize => {
                ContainerQueryRequiredAxes {
                    inline: false,
                    block: true,
                }
            }
        },
        ContainerQueryExpr::Unknown => ContainerQueryRequiredAxes::default(),
        ContainerQueryExpr::Not(inner) => container_query_required_axes(inner),
        ContainerQueryExpr::And(items) | ContainerQueryExpr::Or(items) => {
            items
                .iter()
                .fold(ContainerQueryRequiredAxes::default(), |mut axes, item| {
                    let item = container_query_required_axes(item);
                    axes.inline |= item.inline;
                    axes.block |= item.block;
                    axes
                })
        }
    }
}

fn evaluate_container_query_expr(
    expr: &ContainerQueryExpr,
    container: &ContainerBox,
    root_font_size: f32,
) -> ContainerQueryTruth {
    match expr {
        ContainerQueryExpr::Feature(feature) => {
            let actual = match feature.axis {
                ContainerQueryAxis::Width | ContainerQueryAxis::InlineSize => {
                    container.content_width
                }
                ContainerQueryAxis::Height | ContainerQueryAxis::BlockSize => {
                    container.content_height
                }
            };
            let threshold = match feature.length {
                ContainerQueryLength::Px(value) => value,
                ContainerQueryLength::Em(value) => value * container.font_size,
                ContainerQueryLength::Rem(value) => value * root_font_size,
            };
            if !actual.is_finite() || !threshold.is_finite() {
                return ContainerQueryTruth::Unknown;
            }
            let matches = match feature.comparison {
                ContainerQueryComparison::Min => actual >= threshold,
                ContainerQueryComparison::Max => actual <= threshold,
                ContainerQueryComparison::GreaterThan => actual > threshold,
                ContainerQueryComparison::LessThan => actual < threshold,
                ContainerQueryComparison::Equal => actual == threshold,
            };
            if matches {
                ContainerQueryTruth::True
            } else {
                ContainerQueryTruth::False
            }
        }
        ContainerQueryExpr::Unknown => ContainerQueryTruth::Unknown,
        ContainerQueryExpr::Not(inner) => {
            evaluate_container_query_expr(inner, container, root_font_size).not()
        }
        ContainerQueryExpr::And(items) => {
            let mut truth = ContainerQueryTruth::True;
            for item in items {
                truth = truth.and(evaluate_container_query_expr(
                    item,
                    container,
                    root_font_size,
                ));
                if truth == ContainerQueryTruth::False {
                    break;
                }
            }
            truth
        }
        ContainerQueryExpr::Or(items) => {
            let mut truth = ContainerQueryTruth::False;
            for item in items {
                truth = truth.or(evaluate_container_query_expr(
                    item,
                    container,
                    root_font_size,
                ));
                if truth == ContainerQueryTruth::True {
                    break;
                }
            }
            truth
        }
    }
}

/// An indexed set of author rules ready for fast per-element matching.
pub struct Stylesheet {
    rules: Vec<Rule>,
    /// Rules whose subject can match this stylesheet's featureless shadow
    /// host. Kept separate because a host is outside the shadow tree's normal
    /// selector index and must only be matched with that tree's explicit host
    /// scope.
    host_rules: Vec<usize>,
    /// `::slotted()` rules are matched against assigned light children with
    /// this stylesheet's host supplied as the selector tree scope.
    slotted_rules: Vec<usize>,
    invalidation_map: InvalidationMap,
    registered_custom_properties: HashMap<String, RegisteredCustomProperty>,
    /// Index zero is the unconditional sentinel.
    container_conditions: Vec<ContainerConditionNode>,
    /// Every offset from each `@keyframes` rule. The opacity sampler resolves
    /// property-specific segments at the stylesheet's explicit sample time.
    keyframes: HashMap<String, Keyframes>,
    animation_sample_time: crate::AnimationSampleTime,
    by_root: Vec<usize>,
    by_id: HashMap<String, Vec<usize>>,
    by_class: HashMap<String, Vec<usize>>,
    by_attribute: HashMap<String, Vec<usize>>,
    by_local: HashMap<String, Vec<usize>>,
    universal: Vec<usize>,
    candidate_slot_count: usize,
    /// `sel::before` / `sel::after` rules matched against their ordinary base
    /// selector. Keeping their full declaration cascade supports both literal
    /// generated text and positioned decorative boxes.
    before_rules: PseudoRuleMap,
    after_rules: PseudoRuleMap,
    placeholder_rules: PseudoRuleMap,
}

const MAX_STYLESHEET_CACHE_SOURCE_BYTES: usize = 8 * 1024 * 1024;
const MAX_STYLESHEET_CACHE_RULES: usize = 100_000;

/// One document-local compiled author stylesheet.
///
/// DOM mutations still run the complete cascade and layout against the live
/// tree. This cache retains only source parsing, selector compilation, and
/// candidate indexing, whose inputs are the ordered CSS text, viewport, and
/// selected CSS media type.
/// Keeping a single exact-key entry prevents cross-document growth and avoids
/// hash-collision correctness risks. Pathological source sets above the byte
/// bound are parsed normally but never retained.
#[derive(Default)]
pub struct StylesheetCache {
    entry: Option<CachedStylesheet>,
    hits: u64,
    misses: u64,
}

struct CachedStylesheet {
    sources: Vec<String>,
    viewport_bits: (u32, u32),
    media_type: CssMediaType,
    source_bytes: usize,
    sheet: Arc<Stylesheet>,
}

impl StylesheetCache {
    pub(crate) fn get_or_parse(
        &mut self,
        tree: &DomTree,
        sources: &[String],
        viewport: (f32, f32),
        media_type: CssMediaType,
    ) -> (Arc<Stylesheet>, bool) {
        let viewport_bits = (viewport.0.to_bits(), viewport.1.to_bits());
        if let Some(entry) = self.entry.as_ref() {
            if entry.viewport_bits == viewport_bits
                && entry.media_type == media_type
                && entry.sources == sources
            {
                self.hits = self.hits.saturating_add(1);
                return (Arc::clone(&entry.sheet), true);
            }
        }

        self.misses = self.misses.saturating_add(1);
        let sheet = Arc::new(Stylesheet::parse_for_viewport_and_media(
            tree,
            sources,
            viewport,
            media_type,
        ));
        let source_bytes = sources
            .iter()
            .try_fold(0usize, |total, source| total.checked_add(source.len()));
        let compiled_rules = sheet.rules.len()
            + sheet.before_rules.rules.len()
            + sheet.after_rules.rules.len()
            + sheet.placeholder_rules.rules.len();
        if source_bytes.is_some_and(|bytes| bytes <= MAX_STYLESHEET_CACHE_SOURCE_BYTES)
            && compiled_rules <= MAX_STYLESHEET_CACHE_RULES
        {
            let source_bytes = source_bytes.unwrap_or_default();
            self.entry = Some(CachedStylesheet {
                sources: sources.to_vec(),
                viewport_bits,
                media_type,
                source_bytes,
                sheet: Arc::clone(&sheet),
            });
        } else {
            self.entry = None;
        }
        (sheet, false)
    }

    pub fn hit_count(&self) -> u64 {
        self.hits
    }

    pub fn miss_count(&self) -> u64 {
        self.misses
    }

    pub fn retained_source_bytes(&self) -> usize {
        self.entry.as_ref().map_or(0, |entry| entry.source_bytes)
    }
}

impl Stylesheet {
    /// Dependency metadata for conservative incremental-style invalidation.
    /// Building this map does not itself enable incremental cascade skipping.
    pub fn invalidation_map(&self) -> &InvalidationMap {
        &self.invalidation_map
    }

    /// Until Stage B supplies completed container geometry, preserved
    /// conditional rules remain inactive rather than using viewport geometry.
    fn container_condition_is_active(
        &self,
        id: ContainerConditionId,
        subject: NodeId,
        kind: ContainerQuerySubjectKind,
        evaluator: &mut Option<&mut ContainerQueryEvaluator<'_>>,
    ) -> bool {
        debug_assert!((id.0 as usize) < self.container_conditions.len());
        if id == ContainerConditionId::NONE {
            return true;
        }
        evaluator
            .as_deref_mut()
            .is_some_and(|evaluator| evaluator.condition_matches(self, subject, id, kind))
    }

    pub(crate) fn has_container_queries(&self) -> bool {
        self.rules
            .iter()
            .any(|rule| rule.container_condition_id != ContainerConditionId::NONE)
            || self
                .before_rules
                .rules
                .iter()
                .chain(&self.after_rules.rules)
                .chain(&self.placeholder_rules.rules)
                .any(|rule| rule.container_condition_id != ContainerConditionId::NONE)
    }

    /// Whether `nid` can receive a declaration from any container-conditional
    /// rule in the current DOM state. Retained style uses this to reset only
    /// conditional subjects (and their inherited subtrees) for its query-free
    /// seed pass instead of discarding every clean computed style merely
    /// because the document owns a query container.
    fn bucket_matches_container_query_rule(
        &self,
        bucket: Option<&Vec<usize>>,
        tree: &DomTree,
        matcher: &mut Matcher,
        nid: NodeId,
    ) -> bool {
        bucket.is_some_and(|indices| {
            indices.iter().copied().any(|index| {
                let rule = &self.rules[index];
                rule.container_condition_id != ContainerConditionId::NONE
                    && (rule.candidate_slot == NO_CANDIDATE_SLOT
                        || matcher.mark_candidate(rule.candidate_slot as usize))
                    && matcher.matches(tree, nid, &rule.sel)
            })
        })
    }

    pub(crate) fn node_matches_container_query_rule(
        &self,
        tree: &DomTree,
        matcher: &mut Matcher,
        nid: NodeId,
    ) -> bool {
        if self.candidate_slot_count != 0 {
            matcher.begin_candidate_collection(self.candidate_slot_count);
        }
        let Some(node) = tree.get_node(nid) else {
            return false;
        };
        let normal_match = node.as_element().is_some_and(|element| {
            self.bucket_matches_container_query_rule(
                self.by_local.get(element.local.as_ref()),
                tree,
                matcher,
                nid,
            ) || (is_root_element(tree, nid)
                && self.bucket_matches_container_query_rule(
                    (!self.by_root.is_empty()).then_some(&self.by_root),
                    tree,
                    matcher,
                    nid,
                ))
                || node.get_attribute("id").is_some_and(|id| {
                    self.bucket_matches_container_query_rule(
                        self.by_id.get(id),
                        tree,
                        matcher,
                        nid,
                    )
                })
                || node.get_attribute("class").is_some_and(|classes| {
                    classes.split_whitespace().any(|class| {
                        self.bucket_matches_container_query_rule(
                            self.by_class.get(class),
                            tree,
                            matcher,
                            nid,
                        )
                    })
                })
                || node.attrs().is_some_and(|attributes| {
                    attributes.iter().any(|attribute| {
                        self.bucket_matches_container_query_rule(
                            self.by_attribute.get(attribute.name.local.as_ref()),
                            tree,
                            matcher,
                            nid,
                        )
                    })
                })
                || self.bucket_matches_container_query_rule(
                    (!self.universal.is_empty()).then_some(&self.universal),
                    tree,
                    matcher,
                    nid,
                )
        });
        let supports_placeholder = node.as_element().is_some_and(|element| {
            matches!(element.local.as_ref(), "input" | "textarea")
        });
        normal_match
            || self
                .before_rules
                .node_matches_container_query_rule(tree, matcher, nid)
            || self
                .after_rules
                .node_matches_container_query_rule(tree, matcher, nid)
            || (supports_placeholder
                && self
                    .placeholder_rules
                    .node_matches_container_query_rule(tree, matcher, nid))
    }

    pub(crate) fn container_condition_depth(&self) -> usize {
        self.container_conditions
            .iter()
            .skip(1)
            .map(|node| {
                let mut depth = 1usize;
                let mut parent = node.parent;
                while parent != ContainerConditionId::NONE {
                    depth += 1;
                    parent = self.container_conditions[parent.0 as usize].parent;
                }
                depth
            })
            .max()
            .unwrap_or(0)
    }

    /// Parse and index a set of raw CSS sources (the text of each `<style>`
    /// block, in document order). Selectors that fail to parse are dropped.
    pub fn parse(tree: &DomTree, sources: &[String]) -> Self {
        Self::parse_for_viewport(tree, sources, (1280.0, 720.0))
    }

    /// Parse author CSS for the live CSS viewport. Media queries must use the
    /// same dimensions as layout and page JavaScript; filtering them against a
    /// fixed desktop width made responsive frameworks build one DOM while the
    /// renderer applied another breakpoint.
    pub fn parse_for_viewport(tree: &DomTree, sources: &[String], viewport: (f32, f32)) -> Self {
        Self::parse_for_viewport_and_media(
            tree,
            sources,
            viewport,
            CssMediaType::Screen,
        )
    }

    pub fn parse_for_viewport_and_media(
        tree: &DomTree,
        sources: &[String],
        viewport: (f32, f32),
        media_type: CssMediaType,
    ) -> Self {
        Self::parse_for_viewport_and_media_at_animation_time(
            tree,
            sources,
            viewport,
            media_type,
            crate::AnimationSampleTime::default(),
        )
    }

    pub fn parse_for_viewport_at_animation_time(
        tree: &DomTree,
        sources: &[String],
        viewport: (f32, f32),
        animation_sample_time: crate::AnimationSampleTime,
    ) -> Self {
        Self::parse_for_viewport_and_media_at_animation_time(
            tree,
            sources,
            viewport,
            CssMediaType::Screen,
            animation_sample_time,
        )
    }

    pub fn parse_for_viewport_and_media_at_animation_time(
        tree: &DomTree,
        sources: &[String],
        viewport: (f32, f32),
        media_type: CssMediaType,
        animation_sample_time: crate::AnimationSampleTime,
    ) -> Self {
        let mut sheet = Stylesheet {
            rules: Vec::new(),
            host_rules: Vec::new(),
            slotted_rules: Vec::new(),
            invalidation_map: InvalidationMap::default(),
            registered_custom_properties: HashMap::new(),
            container_conditions: vec![ContainerConditionNode {
                parent: ContainerConditionId::NONE,
                alternatives: Vec::new(),
            }],
            keyframes: HashMap::new(),
            animation_sample_time,
            by_root: Vec::new(),
            by_id: HashMap::new(),
            by_class: HashMap::new(),
            by_attribute: HashMap::new(),
            by_local: HashMap::new(),
            universal: Vec::new(),
            candidate_slot_count: 0,
            before_rules: PseudoRuleMap::default(),
            after_rules: PseudoRuleMap::default(),
            placeholder_rules: PseudoRuleMap::default(),
        };
        let mut order = 0usize;
        let mut layers = LayerRegistry::default();
        let mut keyframe_winners = HashMap::<String, (Option<LayerOrder>, bool, usize)>::new();
        for src in sources {
            let parsed = parse_stylesheet_for_viewport_preserving_containers_in_layer(
                src,
                viewport,
                media_type,
                &mut sheet.container_conditions,
                ContainerConditionId::NONE,
                &mut layers,
                None,
            );
            for ParsedRule {
                selector,
                declarations: decls,
                container_condition_id,
                layer,
            } in parsed
            {
                let keyframe = selector
                    .strip_prefix(KEYFRAMES_SELECTOR_PREFIX)
                    .map(|name| (name, false))
                    .or_else(|| {
                        selector
                            .strip_prefix(WEBKIT_KEYFRAMES_SELECTOR_PREFIX)
                            .map(|name| (name, true))
                    });
                if let Some((name, prefixed)) = keyframe {
                    let replaces = keyframe_winners.get(name).is_none_or(
                        |(winning_layer, winning_prefixed, winning_order)| {
                            compare_layer_order(layer.as_ref(), winning_layer.as_ref())
                                .then_with(|| match (*winning_prefixed, prefixed) {
                                    (true, false) => std::cmp::Ordering::Greater,
                                    (false, true) => std::cmp::Ordering::Less,
                                    _ => order.cmp(winning_order),
                                })
                                .is_gt()
                        },
                    );
                    if replaces {
                        let keyframes = compile_keyframe_body(&decls);
                        sheet.keyframes.insert(name.to_string(), keyframes);
                        keyframe_winners.insert(name.to_string(), (layer.clone(), prefixed, order));
                    }
                    order += 1;
                    continue;
                }
                if let Some(name) = selector.strip_prefix(PROPERTY_REGISTRATION_SELECTOR_PREFIX) {
                    if let Some(registration) = parse_property_registration(&decls) {
                        sheet
                            .registered_custom_properties
                            .insert(name.to_string(), registration);
                    }
                    continue;
                }
                let sel_trim = selector.trim();
                if let Some(base) = strip_pseudo_element(sel_trim, "before") {
                    if let Some(sel) = tree.compile_rule_selector(base) {
                        note_selector_for_invalidation(
                            &mut sheet.invalidation_map,
                            base,
                            order,
                        );
                        note_declaration_attribute_dependencies(
                            &mut sheet.invalidation_map,
                            &decls,
                            order,
                        );
                        let (normal_decls, important_decls) =
                            crate::style::partition_declarations(&decls);
                        let normal_flags = declaration_stream_flags(&normal_decls);
                        let important_flags = declaration_stream_flags(&important_decls);
                        let specificity = sel.specificity();
                        sheet.before_rules.push(PseudoRule {
                            sel,
                            specificity,
                            normal_decls,
                            important_decls,
                            normal_flags,
                            important_flags,
                            candidate_slot: NO_CANDIDATE_SLOT,
                            order,
                            container_condition_id,
                            layer,
                        });
                    } else if selector_requires_conservative_tracking(base) {
                        // Keep correctness metadata for relative/structural
                        // syntax that the current selector matcher cannot yet
                        // compile. Once matcher support lands, phase 2 must not
                        // silently treat the rule as locally invalidatable.
                        note_selector_for_invalidation(
                            &mut sheet.invalidation_map,
                            base,
                            order,
                        );
                    }
                    order += 1;
                    continue;
                }
                if let Some(base) = strip_pseudo_element(sel_trim, "after") {
                    if let Some(sel) = tree.compile_rule_selector(base) {
                        note_selector_for_invalidation(
                            &mut sheet.invalidation_map,
                            base,
                            order,
                        );
                        note_declaration_attribute_dependencies(
                            &mut sheet.invalidation_map,
                            &decls,
                            order,
                        );
                        let (normal_decls, important_decls) =
                            crate::style::partition_declarations(&decls);
                        let normal_flags = declaration_stream_flags(&normal_decls);
                        let important_flags = declaration_stream_flags(&important_decls);
                        let specificity = sel.specificity();
                        sheet.after_rules.push(PseudoRule {
                            sel,
                            specificity,
                            normal_decls,
                            important_decls,
                            normal_flags,
                            important_flags,
                            candidate_slot: NO_CANDIDATE_SLOT,
                            order,
                            container_condition_id,
                            layer,
                        });
                    } else if selector_requires_conservative_tracking(base) {
                        note_selector_for_invalidation(
                            &mut sheet.invalidation_map,
                            base,
                            order,
                        );
                    }
                    order += 1;
                    continue;
                }
                if let Some(base) = strip_pseudo_element(sel_trim, "placeholder") {
                    if let Some(sel) = tree.compile_rule_selector(base) {
                        note_selector_for_invalidation(
                            &mut sheet.invalidation_map,
                            base,
                            order,
                        );
                        note_declaration_attribute_dependencies(
                            &mut sheet.invalidation_map,
                            &decls,
                            order,
                        );
                        let (normal_decls, important_decls) =
                            crate::style::partition_declarations(&decls);
                        let normal_flags = declaration_stream_flags(&normal_decls);
                        let important_flags = declaration_stream_flags(&important_decls);
                        let specificity = sel.specificity();
                        sheet.placeholder_rules.push(PseudoRule {
                            sel,
                            specificity,
                            normal_decls,
                            important_decls,
                            normal_flags,
                            important_flags,
                            candidate_slot: NO_CANDIDATE_SLOT,
                            order,
                            container_condition_id,
                            layer,
                        });
                    } else if selector_requires_conservative_tracking(base) {
                        note_selector_for_invalidation(
                            &mut sheet.invalidation_map,
                            base,
                            order,
                        );
                    }
                    order += 1;
                    continue;
                }
                let Some(sel) = tree.compile_rule_selector(&selector) else {
                    if selector_requires_conservative_tracking(&selector) {
                        note_selector_for_invalidation(
                            &mut sheet.invalidation_map,
                            &selector,
                            order,
                        );
                        order += 1;
                    }
                    continue;
                };
                note_selector_for_invalidation(
                    &mut sheet.invalidation_map,
                    &selector,
                    order,
                );
                note_declaration_attribute_dependencies(
                    &mut sheet.invalidation_map,
                    &decls,
                    order,
                );
                let (normal_decls, important_decls) = crate::style::partition_declarations(&decls);
                let specificity = sel.specificity();
                let normal_flags = declaration_stream_flags(&normal_decls);
                let important_flags = declaration_stream_flags(&important_decls);
                let idx = sheet.rules.len();
                if sel.matches_featureless_host() {
                    sheet.host_rules.push(idx);
                }
                if sel.is_slotted() {
                    sheet.slotted_rules.push(idx);
                }
                let candidate_slot = if sel.candidate_keys().len() > 1 {
                    let slot = u32::try_from(sheet.candidate_slot_count)
                        .expect("selector candidate slot count exceeds u32");
                    sheet.candidate_slot_count += 1;
                    slot
                } else {
                    NO_CANDIDATE_SLOT
                };
                for key in sel.candidate_keys() {
                    match key {
                        SelectorKey::Root => sheet.by_root.push(idx),
                        SelectorKey::Id(v) => sheet.by_id.entry(v.clone()).or_default().push(idx),
                        SelectorKey::Class(v) => {
                            sheet.by_class.entry(v.clone()).or_default().push(idx)
                        }
                        SelectorKey::Attribute(v) => {
                            sheet.by_attribute.entry(v.clone()).or_default().push(idx)
                        }
                        SelectorKey::Local(v) => {
                            sheet.by_local.entry(v.clone()).or_default().push(idx)
                        }
                        SelectorKey::Universal => sheet.universal.push(idx),
                    }
                }
                sheet.rules.push(Rule {
                    sel,
                    specificity,
                    normal_decls,
                    important_decls,
                    normal_flags,
                    important_flags,
                    candidate_slot,
                    order,
                    container_condition_id,
                    layer,
                });
                order += 1;
            }
        }
        sheet
    }

    pub fn pseudo_styles(
        &self,
        tree: &DomTree,
        matcher: &mut Matcher,
        nid: NodeId,
        props: &HashMap<String, String>,
        host_style: &LayoutStyle,
    ) -> (Option<LayoutStyle>, Option<LayoutStyle>) {
        let (before, after, _) =
            self.pseudo_styles_internal(tree, matcher, nid, props, host_style, None);
        (before, after)
    }

    pub(crate) fn all_pseudo_styles(
        &self,
        tree: &DomTree,
        matcher: &mut Matcher,
        nid: NodeId,
        props: &HashMap<String, String>,
        host_style: &LayoutStyle,
        evaluator: Option<&mut ContainerQueryEvaluator<'_>>,
    ) -> (
        Option<LayoutStyle>,
        Option<LayoutStyle>,
        Option<LayoutStyle>,
    ) {
        self.pseudo_styles_internal(tree, matcher, nid, props, host_style, evaluator)
    }

    fn pseudo_styles_internal(
        &self,
        tree: &DomTree,
        matcher: &mut Matcher,
        nid: NodeId,
        props: &HashMap<String, String>,
        host_style: &LayoutStyle,
        mut evaluator: Option<&mut ContainerQueryEvaluator<'_>>,
    ) -> (
        Option<LayoutStyle>,
        Option<LayoutStyle>,
        Option<LayoutStyle>,
    ) {
        let mut build = |rules: &PseudoRuleMap, matcher: &mut Matcher, is_placeholder: bool| {
            let mut normal_matched: Vec<(u32, usize, usize)> = Vec::new();
            let mut important_matched: Vec<(u32, usize, usize)> = Vec::new();
            if rules.candidate_slot_count != 0 {
                matcher.begin_candidate_collection(rules.candidate_slot_count);
            }
            let mut consider =
                |bucket: Option<&Vec<usize>>,
                 normal_matched: &mut Vec<(u32, usize, usize)>,
                 important_matched: &mut Vec<(u32, usize, usize)>| {
                    let Some(indices) = bucket else { return };
                    for &index in indices {
                        let rule = &rules.rules[index];
                        if rule.candidate_slot != NO_CANDIDATE_SLOT
                            && !matcher.mark_candidate(rule.candidate_slot as usize)
                        {
                            continue;
                        }
                        // Candidate buckets only reject impossible originating
                        // elements. Full selector matching remains authoritative.
                        // Container lookup is an ancestor walk, so keep it behind
                        // the selector match as well.
                        if matcher.matches(tree, nid, &rule.sel)
                            && self.container_condition_is_active(
                                rule.container_condition_id,
                                nid,
                                ContainerQuerySubjectKind::OriginatingPseudo,
                                &mut evaluator,
                            )
                        {
                            let matched = (rule.specificity, rule.order, index);
                            if !rule.normal_decls.is_empty() {
                                normal_matched.push(matched);
                            }
                            if !rule.important_decls.is_empty() {
                                important_matched.push(matched);
                            }
                        }
                    }
                };

            if let Some(node) = tree.get_node(nid) {
                if let Some(element) = node.as_element() {
                    consider(
                        rules.by_local.get(element.local.as_ref()),
                        &mut normal_matched,
                        &mut important_matched,
                    );
                }
                if let Some(id) = node.get_attribute("id") {
                    consider(
                        rules.by_id.get(id),
                        &mut normal_matched,
                        &mut important_matched,
                    );
                }
                if let Some(classes) = node.get_attribute("class") {
                    for class in classes.split_whitespace() {
                        consider(
                            rules.by_class.get(class),
                            &mut normal_matched,
                            &mut important_matched,
                        );
                    }
                }
                if !rules.by_attribute.is_empty() {
                    if let Some(attributes) = node.attrs() {
                        for attribute in attributes {
                            consider(
                                rules.by_attribute.get(attribute.name.local.as_ref()),
                                &mut normal_matched,
                                &mut important_matched,
                            );
                        }
                    }
                }
            }
            if is_root_element(tree, nid) {
                consider(
                    (!rules.by_root.is_empty()).then_some(&rules.by_root),
                    &mut normal_matched,
                    &mut important_matched,
                );
            }
            consider(
                (!rules.universal.is_empty()).then_some(&rules.universal),
                &mut normal_matched,
                &mut important_matched,
            );

            if normal_matched.is_empty() && important_matched.is_empty() {
                return None;
            }
            if normal_matched.len() > 1 {
                normal_matched.sort_unstable_by(|a, b| {
                    compare_rule_cascade(
                        rules.rules[a.2].layer.as_ref(),
                        a.0,
                        a.1,
                        rules.rules[b.2].layer.as_ref(),
                        b.0,
                        b.1,
                        false,
                    )
                });
            }
            if important_matched.len() > 1 {
                important_matched.sort_unstable_by(|a, b| {
                    compare_rule_cascade(
                        rules.rules[a.2].layer.as_ref(),
                        a.0,
                        a.1,
                        rules.rules[b.2].layer.as_ref(),
                        b.0,
                        b.1,
                        true,
                    )
                });
            }
            // Generated ::before/::after boxes have an inline outer display
            // by default. LayoutStyle's general default is block because it
            // primarily represents ordinary DOM boxes, so set the pseudo
            // initial value explicitly before applying author declarations.
            let mut style = LayoutStyle {
                display: crate::Display::Inline,
                ..Default::default()
            };
            style.color_scheme_dark = host_style.color_scheme_dark;
            if is_placeholder {
                // Chromium's light native-control placeholder color. Author
                // declarations cascade over this UA-origin initial value.
                style.color = Some([117, 117, 117, 255]);
            }
            let inherited_color_scheme_dark = host_style.color_scheme_dark;
            let mut generated_content = None;
            for &(_, _, index) in &normal_matched {
                let rule = &rules.rules[index];
                if !rule.normal_flags.has_color_scheme {
                    continue;
                }
                let expanded =
                    substitute_declarations(&rule.normal_decls, props, rule.normal_flags.has_var);
                crate::style::apply_color_scheme_declarations_from(
                    &mut style,
                    &expanded,
                    inherited_color_scheme_dark,
                );
            }
            for &(_, _, index) in &important_matched {
                let rule = &rules.rules[index];
                if !rule.important_flags.has_color_scheme {
                    continue;
                }
                let expanded = substitute_declarations(
                    &rule.important_decls,
                    props,
                    rule.important_flags.has_var,
                );
                crate::style::apply_color_scheme_declarations_from(
                    &mut style,
                    &expanded,
                    inherited_color_scheme_dark,
                );
            }
            for &(_, _, index) in &normal_matched {
                let rule = &rules.rules[index];
                let expanded =
                    substitute_declarations(&rule.normal_decls, props, rule.normal_flags.has_var);
                crate::style::apply_declarations_with_locked_color_scheme(&mut style, &expanded);
                if let Some(value) = extract_content(&expanded, tree, nid) {
                    generated_content = value;
                }
            }
            for &(_, _, index) in &important_matched {
                let rule = &rules.rules[index];
                let expanded = substitute_declarations(
                    &rule.important_decls,
                    props,
                    rule.important_flags.has_var,
                );
                crate::style::apply_declarations_with_locked_color_scheme(&mut style, &expanded);
                if let Some(value) = extract_content(&expanded, tree, nid) {
                    generated_content = value;
                }
            }
            style.before_content = generated_content
                .as_ref()
                .map(|items| generated_content_with_zero_counters(items));
            style.generated_content = generated_content;
            if is_placeholder {
                // `color` is inherited on the pseudo. The declaration parser
                // represents `inherit` as None, so resolve it against the
                // originating control after the author cascade.
                if style.color.is_none() {
                    style.color = host_style.color;
                }
                Some(style)
            } else if style.generated_content.is_some() || style.content_image.is_some() {
                Some(style)
            } else {
                None
            }
        };
        let supports_placeholder = tree
            .get_node(nid)
            .is_some_and(|node| {
                node.as_element().is_some_and(|element| {
                    matches!(element.local.as_ref(), "input" | "textarea")
                })
            });
        (
            build(&self.before_rules, matcher, false),
            build(&self.after_rules, matcher, false),
            supports_placeholder
                .then(|| build(&self.placeholder_rules, matcher, true))
                .flatten(),
        )
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    #[doc(hidden)]
    pub fn debug_stats(&self) -> (usize, usize, usize, usize, usize, usize) {
        (
            self.rules.len(),
            self.by_id.len(),
            self.by_class.len(),
            self.by_attribute.len(),
            self.by_local.len(),
            self.universal.len(),
        )
    }

    fn shadow_host_declarations(
        &self,
        tree: &DomTree,
        matcher: &mut Matcher,
        host: NodeId,
        mut evaluator: Option<&mut ContainerQueryEvaluator<'_>>,
    ) -> ShadowScopeDeclarations {
        let mut normal = Vec::new();
        let mut important = Vec::new();
        for &index in &self.host_rules {
            let rule = &self.rules[index];
            if matcher.matches_shadow_host(tree, host, &rule.sel, host)
                && self.container_condition_is_active(
                    rule.container_condition_id,
                    host,
                    ContainerQuerySubjectKind::Element,
                    &mut evaluator,
                )
            {
                let matched = (rule.specificity, rule.order, index);
                if !rule.normal_decls.is_empty() {
                    normal.push(matched);
                }
                if !rule.important_decls.is_empty() {
                    important.push(matched);
                }
            }
        }
        normal.sort_unstable_by(|a, b| {
            compare_rule_cascade(
                self.rules[a.2].layer.as_ref(),
                a.0,
                a.1,
                self.rules[b.2].layer.as_ref(),
                b.0,
                b.1,
                false,
            )
        });
        important.sort_unstable_by(|a, b| {
            compare_rule_cascade(
                self.rules[a.2].layer.as_ref(),
                a.0,
                a.1,
                self.rules[b.2].layer.as_ref(),
                b.0,
                b.1,
                true,
            )
        });

        let mut declarations = ShadowScopeDeclarations::default();
        for &(_, _, index) in &normal {
            append_declaration_stream(&mut declarations.normal, &self.rules[index].normal_decls);
        }
        for &(_, _, index) in &important {
            append_declaration_stream(
                &mut declarations.important,
                &self.rules[index].important_decls,
            );
        }
        declarations
    }

    fn shadow_slotted_declarations(
        &self,
        tree: &DomTree,
        matcher: &mut Matcher,
        subject: NodeId,
        host: NodeId,
        mut evaluator: Option<&mut ContainerQueryEvaluator<'_>>,
    ) -> ShadowScopeDeclarations {
        let mut normal = Vec::new();
        let mut important = Vec::new();
        for &index in &self.slotted_rules {
            let rule = &self.rules[index];
            if matcher.matches_in_shadow_scope(tree, subject, &rule.sel, host)
                && self.container_condition_is_active(
                    rule.container_condition_id,
                    subject,
                    ContainerQuerySubjectKind::Element,
                    &mut evaluator,
                )
            {
                let matched = (rule.specificity, rule.order, index);
                if !rule.normal_decls.is_empty() {
                    normal.push(matched);
                }
                if !rule.important_decls.is_empty() {
                    important.push(matched);
                }
            }
        }
        normal.sort_unstable_by(|a, b| {
            compare_rule_cascade(
                self.rules[a.2].layer.as_ref(),
                a.0,
                a.1,
                self.rules[b.2].layer.as_ref(),
                b.0,
                b.1,
                false,
            )
        });
        important.sort_unstable_by(|a, b| {
            compare_rule_cascade(
                self.rules[a.2].layer.as_ref(),
                a.0,
                a.1,
                self.rules[b.2].layer.as_ref(),
                b.0,
                b.1,
                true,
            )
        });

        let mut declarations = ShadowScopeDeclarations::default();
        for &(_, _, index) in &normal {
            append_declaration_stream(&mut declarations.normal, &self.rules[index].normal_decls);
        }
        for &(_, _, index) in &important {
            append_declaration_stream(
                &mut declarations.important,
                &self.rules[index].important_decls,
            );
        }
        declarations
    }

    /// Apply every author rule that matches `nid` to `style`, in cascade order
    /// (ascending specificity, then source order, so the winner is applied last).
    /// `id`, `classes`, and `local` are the element's precomputed keys.
    /// `parent_props` is the element's inherited custom-property map. Returns
    /// `Some(map)` (parent + this element's own `--x` declarations) when this
    /// element declares any custom properties, so the caller can thread the
    /// richer map to descendants; `None` means "reuse the parent's map".
    pub fn apply(
        &self,
        tree: &DomTree,
        matcher: &mut Matcher,
        nid: NodeId,
        id: Option<&str>,
        classes: &[String],
        local: &str,
        style: &mut LayoutStyle,
        parent_props: &HashMap<String, String>,
        inline_css: Option<&str>,
    ) -> Option<HashMap<String, String>> {
        let mut animation_timeline = crate::AnimationTimelineState::default();
        self.apply_at_animation_time(
            tree,
            matcher,
            nid,
            id,
            classes,
            local,
            style,
            parent_props,
            inline_css,
            crate::AnimationSample {
                time: self.animation_sample_time,
                mode: crate::AnimationSampleMode::DocumentTime,
            },
            &mut animation_timeline,
        )
    }

    pub(crate) fn apply_at_animation_time(
        &self,
        tree: &DomTree,
        matcher: &mut Matcher,
        nid: NodeId,
        id: Option<&str>,
        classes: &[String],
        local: &str,
        style: &mut LayoutStyle,
        parent_props: &HashMap<String, String>,
        inline_css: Option<&str>,
        animation_sample: crate::AnimationSample,
        animation_timeline: &mut crate::AnimationTimelineState,
    ) -> Option<HashMap<String, String>> {
        self.apply_internal(
            tree,
            matcher,
            nid,
            id,
            classes,
            local,
            style,
            parent_props,
            inline_css,
            None,
            &[],
            None,
            animation_sample,
            animation_timeline,
        )
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn apply_with_container_queries(
        &self,
        tree: &DomTree,
        matcher: &mut Matcher,
        nid: NodeId,
        id: Option<&str>,
        classes: &[String],
        local: &str,
        style: &mut LayoutStyle,
        parent_props: &HashMap<String, String>,
        inline_css: Option<&str>,
        evaluator: &mut ContainerQueryEvaluator<'_>,
    ) -> Option<HashMap<String, String>> {
        let mut animation_timeline = crate::AnimationTimelineState::default();
        self.apply_with_container_queries_at_animation_time(
            tree,
            matcher,
            nid,
            id,
            classes,
            local,
            style,
            parent_props,
            inline_css,
            evaluator,
            crate::AnimationSample {
                time: self.animation_sample_time,
                mode: crate::AnimationSampleMode::DocumentTime,
            },
            &mut animation_timeline,
        )
    }

    pub(crate) fn apply_with_container_queries_at_animation_time(
        &self,
        tree: &DomTree,
        matcher: &mut Matcher,
        nid: NodeId,
        id: Option<&str>,
        classes: &[String],
        local: &str,
        style: &mut LayoutStyle,
        parent_props: &HashMap<String, String>,
        inline_css: Option<&str>,
        evaluator: &mut ContainerQueryEvaluator<'_>,
        animation_sample: crate::AnimationSample,
        animation_timeline: &mut crate::AnimationTimelineState,
    ) -> Option<HashMap<String, String>> {
        self.apply_internal(
            tree,
            matcher,
            nid,
            id,
            classes,
            local,
            style,
            parent_props,
            inline_css,
            None,
            &[],
            Some(evaluator),
            animation_sample,
            animation_timeline,
        )
    }

    /// Apply document author rules together with `:host` and `::slotted()`
    /// rules from the element's applicable shadow scopes. Encapsulation
    /// context precedes specificity and layer order: shadow normal declarations
    /// are weaker than document/inline normal declarations, while shadow
    /// `!important` is stronger than document/inline author-important.
    pub(crate) fn apply_with_shadow_scopes_at_animation_time(
        &self,
        shadow_host_sheet: Option<&Stylesheet>,
        slotted_scopes: &[ShadowSlottedScope<'_>],
        tree: &DomTree,
        matcher: &mut Matcher,
        nid: NodeId,
        id: Option<&str>,
        classes: &[String],
        local: &str,
        style: &mut LayoutStyle,
        parent_props: &HashMap<String, String>,
        inline_css: Option<&str>,
        evaluator: Option<&mut ContainerQueryEvaluator<'_>>,
        animation_sample: crate::AnimationSample,
        animation_timeline: &mut crate::AnimationTimelineState,
    ) -> Option<HashMap<String, String>> {
        self.apply_internal(
            tree,
            matcher,
            nid,
            id,
            classes,
            local,
            style,
            parent_props,
            inline_css,
            shadow_host_sheet,
            slotted_scopes,
            evaluator,
            animation_sample,
            animation_timeline,
        )
    }

    fn apply_internal(
        &self,
        tree: &DomTree,
        matcher: &mut Matcher,
        nid: NodeId,
        id: Option<&str>,
        classes: &[String],
        local: &str,
        style: &mut LayoutStyle,
        parent_props: &HashMap<String, String>,
        inline_css: Option<&str>,
        shadow_host_sheet: Option<&Stylesheet>,
        slotted_scopes: &[ShadowSlottedScope<'_>],
        mut evaluator: Option<&mut ContainerQueryEvaluator<'_>>,
        animation_sample: crate::AnimationSample,
        animation_timeline: &mut crate::AnimationTimelineState,
    ) -> Option<HashMap<String, String>> {
        let shadow_host_declarations = shadow_host_sheet
            .map(|sheet| {
                sheet.shadow_host_declarations(tree, matcher, nid, evaluator.as_deref_mut())
            })
            .unwrap_or_default();
        let mut shadow_scope_declarations = ShadowScopeDeclarations::default();
        append_declaration_stream(
            &mut shadow_scope_declarations.normal,
            &shadow_host_declarations.normal,
        );
        let mut slotted_declarations = Vec::with_capacity(slotted_scopes.len());
        for scope in slotted_scopes {
            slotted_declarations.push(scope.sheet.shadow_slotted_declarations(
                tree,
                matcher,
                nid,
                scope.host,
                evaluator.as_deref_mut(),
            ));
        }
        // Match Gecko's ShadowCascadeOrder: normal declarations progress from
        // the host's own (innermost) tree through outer slot scopes toward the
        // document. Important declarations reverse that order.
        for declarations in slotted_declarations.iter().rev() {
            append_declaration_stream(
                &mut shadow_scope_declarations.normal,
                &declarations.normal,
            );
        }
        for declarations in &slotted_declarations {
            append_declaration_stream(
                &mut shadow_scope_declarations.important,
                &declarations.important,
            );
        }
        append_declaration_stream(
            &mut shadow_scope_declarations.important,
            &shadow_host_declarations.important,
        );
        // Keep the two cascade priorities separate from the outset. A typical
        // stylesheet has very few important declarations, so cloning and
        // sorting every matching normal-only rule into an empty important pass
        // is substantial wasted work on every element.
        let mut normal_matched: Vec<(u32, usize, usize)> = Vec::new();
        let mut important_matched: Vec<(u32, usize, usize)> = Vec::new();
        if self.candidate_slot_count != 0 {
            matcher.begin_candidate_collection(self.candidate_slot_count);
        }
        let mut consider =
            |bucket: Option<&Vec<usize>>,
             normal_matched: &mut Vec<(u32, usize, usize)>,
             important_matched: &mut Vec<(u32, usize, usize)>| {
                if let Some(idxs) = bucket {
                    for &i in idxs {
                        let rule = &self.rules[i];
                        if rule.candidate_slot != NO_CANDIDATE_SLOT
                            && !matcher.mark_candidate(rule.candidate_slot as usize)
                        {
                            continue;
                        }
                        // Container lookup is an ancestor walk. Keep it behind
                        // selector matching and cache the result per condition.
                        if matcher.matches(tree, nid, &rule.sel)
                            && self.container_condition_is_active(
                                rule.container_condition_id,
                                nid,
                                ContainerQuerySubjectKind::Element,
                                &mut evaluator,
                            )
                        {
                            let matched = (rule.specificity, rule.order, i);
                            if !rule.normal_decls.is_empty() {
                                normal_matched.push(matched);
                            }
                            if !rule.important_decls.is_empty() {
                                important_matched.push(matched);
                            }
                        }
                    }
                }
            };

        consider(
            self.by_local.get(local),
            &mut normal_matched,
            &mut important_matched,
        );
        if is_root_element(tree, nid) {
            consider(
                (!self.by_root.is_empty()).then_some(&self.by_root),
                &mut normal_matched,
                &mut important_matched,
            );
        }
        if let Some(id) = id {
            consider(
                self.by_id.get(id),
                &mut normal_matched,
                &mut important_matched,
            );
        }
        for c in classes {
            consider(
                self.by_class.get(c),
                &mut normal_matched,
                &mut important_matched,
            );
        }
        if !self.by_attribute.is_empty() {
            if let Some(node) = tree.get_node(nid) {
                if let Some(attributes) = node.attrs() {
                    for attribute in attributes {
                        consider(
                            self.by_attribute.get(attribute.name.local.as_ref()),
                            &mut normal_matched,
                            &mut important_matched,
                        );
                    }
                }
            }
        }
        if !self.universal.is_empty() {
            consider(
                Some(&self.universal),
                &mut normal_matched,
                &mut important_matched,
            );
        }

        if normal_matched.len() > 1 {
            normal_matched.sort_unstable_by(|a, b| {
                let left = &self.rules[a.2];
                let right = &self.rules[b.2];
                compare_rule_cascade(
                    left.layer.as_ref(),
                    a.0,
                    a.1,
                    right.layer.as_ref(),
                    b.0,
                    b.1,
                    false,
                )
            });
        }
        if important_matched.len() > 1 {
            important_matched.sort_unstable_by(|a, b| {
                let left = &self.rules[a.2];
                let right = &self.rules[b.2];
                compare_rule_cascade(
                    left.layer.as_ref(),
                    a.0,
                    a.1,
                    right.layer.as_ref(),
                    b.0,
                    b.1,
                    true,
                )
            });
        }

        let (inline_normal, inline_important) = inline_css
            .map(crate::style::partition_declarations)
            .unwrap_or_default();

        // Pass 1: collect this element's own custom properties (`--x: value`),
        // in cascade order (last wins), layered over the inherited map. Custom
        // properties cascade fully before any `var()` is substituted.
        let inline_normal_flags = declaration_stream_flags(&inline_normal);
        let inline_important_flags = declaration_stream_flags(&inline_important);
        let shadow_normal_flags = declaration_stream_flags(&shadow_scope_declarations.normal);
        let shadow_important_flags = declaration_stream_flags(&shadow_scope_declarations.important);
        let has_own_custom_properties = shadow_normal_flags.has_custom_properties
            || shadow_important_flags.has_custom_properties
            || inline_normal_flags.has_custom_properties
            || inline_important_flags.has_custom_properties
            || normal_matched
                .iter()
                .any(|&(_, _, i)| self.rules[i].normal_flags.has_custom_properties)
            || important_matched
                .iter()
                .any(|&(_, _, i)| self.rules[i].important_flags.has_custom_properties);

        // Registered properties are already represented in the parent's
        // computed map. Most descendants need no changes: inherited
        // registrations keep that value, while a non-inherited registration
        // whose parent already holds its initial value also stays identical.
        // Detect the uncommon transition before cloning the potentially large
        // custom-property map. This follows the browser rule-tree model where
        // unchanged computed values are shared down the tree.
        let registrations_change_parent = self.registered_custom_properties.iter().any(
            |(name, registration)| {
                if registration.inherits && parent_props.contains_key(name) {
                    return false;
                }
                match &registration.initial_value {
                    Some(initial) => parent_props.get(name) != Some(initial),
                    None => parent_props.contains_key(name),
                }
            },
        );
        let effective = if !has_own_custom_properties && !registrations_change_parent {
            None
        } else {
            let mut own: Vec<(String, String)> = Vec::new();
            let mut collect_custom = |css: &str| {
                for decl in crate::style::split_declarations(css) {
                    if let Some((name, val)) = decl.split_once(':') {
                        let name = name.trim();
                        if name.starts_with("--") && name.len() > 2 {
                            own.push((name.to_string(), val.trim().to_string()));
                        }
                    }
                }
            };
            if shadow_normal_flags.has_custom_properties {
                collect_custom(&shadow_scope_declarations.normal);
            }
            for &(_, _, i) in &normal_matched {
                let rule = &self.rules[i];
                if rule.normal_flags.has_custom_properties {
                    collect_custom(&rule.normal_decls);
                }
            }
            if inline_normal_flags.has_custom_properties {
                collect_custom(&inline_normal);
            }
            for &(_, _, i) in &important_matched {
                let rule = &self.rules[i];
                if rule.important_flags.has_custom_properties {
                    collect_custom(&rule.important_decls);
                }
            }
            if inline_important_flags.has_custom_properties {
                collect_custom(&inline_important);
            }
            if shadow_important_flags.has_custom_properties {
                collect_custom(&shadow_scope_declarations.important);
            }

            let mut resolved_props = parent_props.clone();
            for (name, registration) in &self.registered_custom_properties {
                if registration.inherits && resolved_props.contains_key(name) {
                    continue;
                }
                if let Some(initial) = &registration.initial_value {
                    resolved_props.insert(name.clone(), initial.clone());
                } else {
                    resolved_props.remove(name);
                }
            }
            let registration_changed_props = registrations_change_parent;
            let has_own = !own.is_empty();
            let mut own_names = std::collections::HashSet::new();
            for (k, v) in own {
                own_names.insert(k.clone());
                let registration = self.registered_custom_properties.get(&k);
                let set_initial = |props: &mut HashMap<String, String>| {
                    if let Some(initial) =
                        registration.and_then(|entry| entry.initial_value.as_ref())
                    {
                        props.insert(k.clone(), initial.clone());
                    } else {
                        props.remove(&k);
                    }
                };
                let inherit = |props: &mut HashMap<String, String>| {
                    if let Some(inherited) = parent_props.get(&k) {
                        props.insert(k.clone(), inherited.clone());
                    } else if let Some(initial) =
                        registration.and_then(|entry| entry.initial_value.as_ref())
                    {
                        props.insert(k.clone(), initial.clone());
                    } else {
                        props.remove(&k);
                    }
                };
                match v.trim().to_ascii_lowercase().as_str() {
                    "initial" => set_initial(&mut resolved_props),
                    "inherit" => inherit(&mut resolved_props),
                    "unset" | "revert" | "revert-layer"
                        if registration.is_some_and(|entry| !entry.inherits) =>
                    {
                        set_initial(&mut resolved_props);
                    }
                    "unset" | "revert" | "revert-layer" => inherit(&mut resolved_props),
                    _ => {
                        let valid = registration.is_none_or(|entry| {
                            substitute_var_value(&v, &resolved_props, 0)
                                .is_some_and(|value| registered_value_matches(entry, &value))
                        });
                        if valid {
                            resolved_props.insert(k, v);
                        } else {
                            set_initial(&mut resolved_props);
                        }
                    }
                }
            }
            // Custom properties inherit their computed value, not their original
            // token stream. Resolve only declarations won on this element against
            // the complete same-element environment (so forward references work),
            // then pass those substituted values to descendants. Re-resolving an
            // inherited `--b:var(--a)` after a child overrides `--a` is observably
            // wrong: browsers keep the parent's already-computed `--b`.
            let resolution_environment = resolved_props.clone();
            for name in own_names {
                let Some(value) = resolution_environment.get(&name) else {
                    continue;
                };
                if let Some(computed) = substitute_var_value(value, &resolution_environment, 0) {
                    resolved_props.insert(name, computed);
                } else if let Some(initial) = self
                    .registered_custom_properties
                    .get(&name)
                    .and_then(|entry| entry.initial_value.clone())
                {
                    resolved_props.insert(name, initial);
                } else {
                    resolved_props.remove(&name);
                }
            }
            if has_own || registration_changed_props {
                Some(resolved_props)
            } else {
                None
            }
        };
        let props = effective.as_ref().unwrap_or(parent_props);

        let inherited_color_scheme_dark = style.color_scheme_dark;
        // `light-dark()` resolves against the element's final used color
        // scheme, not the declaration order. Determine the scheme winner
        // across the complete author cascade before applying any color-valued
        // property. The style starts with its inherited scheme.
        if shadow_normal_flags.has_color_scheme {
            let expanded = substitute_declarations(
                &shadow_scope_declarations.normal,
                props,
                shadow_normal_flags.has_var,
            );
            crate::style::apply_color_scheme_declarations_from(
                style,
                &expanded,
                inherited_color_scheme_dark,
            );
        }
        for &(_, _, i) in &normal_matched {
            let rule = &self.rules[i];
            if !rule.normal_flags.has_color_scheme {
                continue;
            }
            let expanded =
                substitute_declarations(&rule.normal_decls, props, rule.normal_flags.has_var);
            crate::style::apply_color_scheme_declarations_from(
                style,
                &expanded,
                inherited_color_scheme_dark,
            );
        }
        if inline_normal_flags.has_color_scheme {
            let expanded =
                substitute_declarations(&inline_normal, props, inline_normal_flags.has_var);
            crate::style::apply_color_scheme_declarations_from(
                style,
                &expanded,
                inherited_color_scheme_dark,
            );
        }
        for &(_, _, i) in &important_matched {
            let rule = &self.rules[i];
            if !rule.important_flags.has_color_scheme {
                continue;
            }
            let expanded =
                substitute_declarations(&rule.important_decls, props, rule.important_flags.has_var);
            crate::style::apply_color_scheme_declarations_from(
                style,
                &expanded,
                inherited_color_scheme_dark,
            );
        }
        if inline_important_flags.has_color_scheme {
            let expanded =
                substitute_declarations(&inline_important, props, inline_important_flags.has_var);
            crate::style::apply_color_scheme_declarations_from(
                style,
                &expanded,
                inherited_color_scheme_dark,
            );
        }
        if shadow_important_flags.has_color_scheme {
            let expanded = substitute_declarations(
                &shadow_scope_declarations.important,
                props,
                shadow_important_flags.has_var,
            );
            crate::style::apply_color_scheme_declarations_from(
                style,
                &expanded,
                inherited_color_scheme_dark,
            );
        }

        // Pass 2: apply normal declarations with `var()` substituted against
        // the resolved custom-property map.
        let expanded = substitute_declarations(
            &shadow_scope_declarations.normal,
            props,
            shadow_normal_flags.has_var,
        );
        crate::style::apply_declarations_with_locked_color_scheme(style, &expanded);
        for &(_, _, i) in &normal_matched {
            let rule = &self.rules[i];
            let expanded =
                substitute_declarations(&rule.normal_decls, props, rule.normal_flags.has_var);
            crate::style::apply_declarations_with_locked_color_scheme(style, &expanded);
        }
        let expanded = substitute_declarations(&inline_normal, props, inline_normal_flags.has_var);
        crate::style::apply_declarations_with_locked_color_scheme(style, &expanded);

        // Animation control properties from author !important participate in
        // computed timing, while the animated value itself remains below the
        // important author origin. Resolve those controls on a temporary style
        // before sampling, then let the ordinary important pass override the
        // sampled values where appropriate.
        let important_has_animation = shadow_important_flags.has_animation
            || inline_important_flags.has_animation
            || important_matched
                .iter()
                .any(|&(_, _, i)| self.rules[i].important_flags.has_animation);
        style.animation_has_render_effect = false;
        style.animation_effect_impact = crate::AnimationEffectImpact::None;
        if !self.keyframes.is_empty() && (style.animation_name.is_some() || important_has_animation)
        {
            let mut animation_style = style.clone();
            for &(_, _, i) in &important_matched {
                let rule = &self.rules[i];
                if !rule.important_flags.has_animation {
                    continue;
                }
                let expanded = substitute_declarations(
                    &rule.important_decls,
                    props,
                    rule.important_flags.has_var,
                );
                crate::style::apply_animation_declarations(&mut animation_style, &expanded);
            }
            if inline_important_flags.has_animation {
                let expanded = substitute_declarations(
                    &inline_important,
                    props,
                    inline_important_flags.has_var,
                );
                crate::style::apply_animation_declarations(&mut animation_style, &expanded);
            }
            if shadow_important_flags.has_animation {
                let expanded = substitute_declarations(
                    &shadow_scope_declarations.important,
                    props,
                    shadow_important_flags.has_var,
                );
                crate::style::apply_animation_declarations(&mut animation_style, &expanded);
            }
            if let Some(name) = animation_style.animation_name.as_deref() {
                if let Some(keyframes) = self.keyframes.get(name) {
                    style.animation_has_render_effect = !keyframes.tracks.is_empty();
                    style.animation_effect_impact = keyframes
                        .tracks
                        .keys()
                        .copied()
                        .map(AnimatedProperty::effect_impact)
                        .max()
                        .unwrap_or_default();
                    let local_sample = animation_timeline.sample_for(
                        nid,
                        crate::AnimationInstanceKey {
                            name: name.to_string(),
                        },
                        animation_style.animation_timing.play_state,
                        animation_sample,
                    );
                    style.animation_local_time_ms = local_sample.milliseconds;
                    sample_animation_properties(
                        keyframes,
                        style,
                        &animation_style.animation_timing,
                        local_sample,
                        props,
                    );
                } else {
                    animation_timeline.clear_animation(nid, animation_sample);
                }
            } else {
                animation_timeline.clear_animation(nid, animation_sample);
            }
        } else {
            animation_timeline.clear_animation(nid, animation_sample);
        }

        // Web Animations contribute at the animation cascade origin: above
        // every normal author declaration (including inline style), but below
        // author !important. Keep the renderer-side effect separate from the
        // authored declaration block so cancel() reveals the exact underlying
        // value and CSSOM never observes a synthetic inline rewrite.
        let has_waapi = animation_timeline
            .waapi_for_node(nid, animation_sample.time)
            .next()
            .is_some();
        let waapi_underlying_transform_ops = has_waapi.then(|| style.transform_ops.clone());
        let waapi_underlying_opacity = style.opacity;
        sample_waapi_properties(animation_timeline, nid, style, animation_sample);

        let important_has_transform = shadow_important_flags.has_transform
            || inline_important_flags.has_transform
            || important_matched
                .iter()
                .any(|&(_, _, i)| self.rules[i].important_flags.has_transform);
        let important_has_opacity = shadow_important_flags.has_opacity
            || inline_important_flags.has_opacity
            || important_matched
                .iter()
                .any(|&(_, _, i)| self.rules[i].important_flags.has_opacity);

        for &(_, _, i) in &important_matched {
            let rule = &self.rules[i];
            let expanded =
                substitute_declarations(&rule.important_decls, props, rule.important_flags.has_var);
            crate::style::apply_declarations_with_locked_color_scheme(style, &expanded);
        }
        let expanded =
            substitute_declarations(&inline_important, props, inline_important_flags.has_var);
        crate::style::apply_declarations_with_locked_color_scheme(style, &expanded);
        let expanded = substitute_declarations(
            &shadow_scope_declarations.important,
            props,
            shadow_important_flags.has_var,
        );
        crate::style::apply_declarations_with_locked_color_scheme(style, &expanded);
        style.waapi_sample_state =
            waapi_underlying_transform_ops.map(|underlying_transform_ops| {
                Box::new(crate::WaapiSampleState {
                    underlying_transform_ops,
                    underlying_opacity: waapi_underlying_opacity,
                    transform_fast_path: !important_has_transform,
                    opacity_fast_path: !important_has_opacity,
                })
            });
        effective
    }
}

/// Replay transform-only Web Animations from their exact underlying cascade
/// value. Returns the sampled operations and transform containing-block bit;
/// callers apply them only after every target has passed preflight.
pub(crate) struct ResampledVisualWaapi {
    pub transform_ops: Vec<crate::TransformOp>,
    pub opacity: Option<f32>,
    pub has_transform_effect: bool,
    pub has_opacity_effect: bool,
    pub establishes_transform_cb: bool,
}

pub(crate) fn resample_visual_waapi(
    timeline: &crate::AnimationTimelineState,
    node: NodeId,
    style: &LayoutStyle,
    sample: crate::AnimationSample,
) -> Option<ResampledVisualWaapi> {
    let retained = style.waapi_sample_state.as_deref()?;
    let animations = timeline
        .waapi_for_node(node, sample.time)
        .collect::<Vec<_>>();
    if animations.is_empty() {
        return None;
    }
    let has_transform_effect = animations.iter().any(|(animation, _)| {
        animation.keyframes.iter().any(|frame| frame.transform.is_some())
    });
    let has_opacity_effect = animations.iter().any(|(animation, _)| {
        animation.keyframes.iter().any(|frame| frame.opacity.is_some())
    });
    let all_effects_supported = animations.iter().all(|(animation, _)| {
        animation
            .keyframes
            .iter()
            .any(|frame| frame.transform.is_some() || frame.opacity.is_some())
    });
    if !all_effects_supported
        || (has_transform_effect && !retained.transform_fast_path)
        || (has_opacity_effect && !retained.opacity_fast_path)
    {
        return None;
    }
    let mut sampled = style.clone();
    sampled.transform_ops = retained.underlying_transform_ops.clone();
    sampled.opacity = retained.underlying_opacity;
    let has_underlying_transform = !sampled.transform_ops.is_empty();
    set_animation_containing_block_trigger(
        &mut sampled,
        crate::CB_TRIGGER_TRANSFORM,
        has_underlying_transform,
    );
    sample_waapi_properties(timeline, node, &mut sampled, sample);
    Some(ResampledVisualWaapi {
        transform_ops: sampled.transform_ops,
        opacity: sampled.opacity,
        has_transform_effect,
        has_opacity_effect,
        establishes_transform_cb: sampled.containing_block_triggers
            & crate::CB_TRIGGER_TRANSFORM
            != 0,
    })
}

fn sample_waapi_properties(
    timeline: &crate::AnimationTimelineState,
    node: NodeId,
    style: &mut LayoutStyle,
    sample: crate::AnimationSample,
) {
    for (animation, local_time) in timeline.waapi_for_node(node, sample.time) {
        let Some(mut progress) = animation_directed_progress(&animation.timing, local_time) else {
            continue;
        };
        if let Some(samples) = animation.linear_easing.as_deref() {
            progress = sample_linear_easing(samples, progress);
        } else if let Some(points) = animation.easing {
            progress = sample_cubic_bezier(points, progress);
        }
        let underlying = style.clone();

        let opacity_track = animation
            .keyframes
            .iter()
            .filter_map(|frame| frame.opacity.map(|value| (frame.offset, value)))
            .collect::<Vec<_>>();
        if let Some(value) = sample_numeric_waapi_track(
            &opacity_track,
            underlying.opacity.unwrap_or(1.0),
            progress,
        ) {
            style.opacity = Some(value.clamp(0.0, 1.0));
        }

        let transform_track = animation
            .keyframes
            .iter()
            .filter_map(|frame| {
                let value = frame.transform.as_ref()?;
                if !crate::style::supports_declaration("transform", value) {
                    return None;
                }
                let mut endpoint = underlying.clone();
                crate::style::apply_animation_property_value(&mut endpoint, "transform", value);
                Some((frame.offset, endpoint.transform_ops))
            })
            .collect::<Vec<_>>();
        if let Some(value) = sample_transform_waapi_track(
            &transform_track,
            underlying.transform_ops.clone(),
            progress,
        ) {
            apply_animation_value(
                style,
                AnimatedProperty::Transform,
                AnimationValue::Transform(value),
            );
        }
    }
}

fn sample_linear_easing(samples: &[f32], progress: f32) -> f32 {
    if samples.len() < 2 {
        return progress;
    }
    let scaled = progress.clamp(0.0, 1.0) * (samples.len() - 1) as f32;
    let index = (scaled.floor() as usize).min(samples.len() - 2);
    let local = scaled - index as f32;
    samples[index] + (samples[index + 1] - samples[index]) * local
}

fn sample_cubic_bezier(points: [f32; 4], progress: f32) -> f32 {
    let [x1, y1, x2, y2] = points;
    let target = progress.clamp(0.0, 1.0);
    let component = |t: f32, first: f32, second: f32| {
        let inverse = 1.0 - t;
        3.0 * inverse * inverse * t * first + 3.0 * inverse * t * t * second + t * t * t
    };
    // x control points are constrained to [0,1], so bisection is stable even
    // for flat derivatives at the ends.
    let (mut low, mut high) = (0.0, 1.0);
    for _ in 0..14 {
        let middle = (low + high) * 0.5;
        if component(middle, x1, x2) < target {
            low = middle;
        } else {
            high = middle;
        }
    }
    component((low + high) * 0.5, y1, y2).clamp(0.0, 1.0)
}

fn sample_numeric_waapi_track(
    track: &[(f32, f32)],
    underlying: f32,
    progress: f32,
) -> Option<f32> {
    sample_waapi_track(track, underlying, progress, |from, to, position| {
        from + (to - from) * position
    })
}

fn sample_transform_waapi_track(
    track: &[(f32, Vec<crate::TransformOp>)],
    underlying: Vec<crate::TransformOp>,
    progress: f32,
) -> Option<Vec<crate::TransformOp>> {
    sample_waapi_track(track, underlying, progress, |from, to, position| {
        interpolate_transform_list(from, to, position)
    })
}

fn sample_waapi_track<T: Clone>(
    track: &[(f32, T)],
    underlying: T,
    progress: f32,
    interpolate: impl Fn(&T, &T, f32) -> T,
) -> Option<T> {
    if track.is_empty() {
        return None;
    }
    let mut resolved = track.to_vec();
    resolved.sort_by(|left, right| left.0.total_cmp(&right.0));
    if resolved[0].0 > 0.0 {
        resolved.insert(0, (0.0, underlying.clone()));
    }
    if resolved.last().is_some_and(|stop| stop.0 < 1.0) {
        resolved.push((1.0, underlying));
    }
    // At a duplicate offset the later keyframe is the outgoing value. The
    // first duplicate remains the incoming interpolation endpoint just before
    // the boundary, so do not deduplicate the track.
    if let Some((_, value)) = resolved
        .iter()
        .rev()
        .find(|(offset, _)| progress == *offset)
    {
        return Some(value.clone());
    }
    if progress < resolved[0].0 {
        return Some(resolved[0].1.clone());
    }
    for pair in resolved.windows(2) {
        let (from_offset, from) = (&pair[0].0, &pair[0].1);
        let (to_offset, to) = (&pair[1].0, &pair[1].1);
        if progress <= *to_offset {
            if progress == *to_offset || from_offset == to_offset {
                return Some(to.clone());
            }
            return Some(interpolate(
                from,
                to,
                (progress - *from_offset) / (*to_offset - *from_offset),
            ));
        }
    }
    resolved.last().map(|stop| stop.1.clone())
}

/// Compile a keyframes body into sparse per-property tracks. Values remain in
/// specified form because `var()` and color-scheme resolution are element
/// dependent, but declaration splitting, shorthand-to-longhand membership,
/// offset distribution, and duplicate-offset ordering are all paid once.
fn compile_keyframe_body(css: &str) -> Keyframes {
    let mut stops = Vec::new();
    for (source_order, (selector, declarations)) in
        parse_stylesheet_for_viewport(css, (1280.0, 720.0))
            .into_iter()
            .enumerate()
    {
        for part in selector.split(',') {
            if let Some(offset) = parse_keyframe_offset(part) {
                stops.push(KeyframeStop {
                    offset: Some(offset),
                    declarations: declarations.clone(),
                    source_order,
                });
            }
        }
    }
    if stops.is_empty() {
        return Keyframes::default();
    }

    let mut tracks = HashMap::<AnimatedProperty, Vec<PropertyTrackStop>>::new();
    for (offset, stop) in normalized_keyframe_offsets(&stops) {
        // CSS Animations ignores important declarations in keyframes.
        let (normal, _) = crate::style::partition_declarations(&stop.declarations);
        let mut declarations = HashMap::<AnimatedProperty, AnimatedDeclaration>::new();
        for raw in crate::style::split_declarations(&normal) {
            let Some((name, value)) = raw.trim().split_once(':') else {
                continue;
            };
            let name = name.trim().to_ascii_lowercase();
            let value = value.trim();
            if value.is_empty()
                || (!value.contains("var(") && !supports_animation_declaration(&name, value))
            {
                continue;
            }
            let declaration = AnimatedDeclaration {
                name: name.clone(),
                value: value.to_string(),
            };
            for property in animated_properties_for_declaration(&name) {
                declarations.insert(property, declaration.clone());
            }
        }
        for (property, declaration) in declarations {
            tracks.entry(property).or_default().push(PropertyTrackStop {
                offset,
                source_order: stop.source_order,
                declaration,
            });
        }
    }
    for track in tracks.values_mut() {
        track.sort_by(|left, right| {
            left.offset
                .total_cmp(&right.offset)
                .then(left.source_order.cmp(&right.source_order))
        });
        let mut merged = Vec::<PropertyTrackStop>::with_capacity(track.len());
        for stop in track.drain(..) {
            if merged.last().is_some_and(|last| last.offset == stop.offset) {
                *merged.last_mut().unwrap() = stop;
            } else {
                merged.push(stop);
            }
        }
        *track = merged;
    }
    Keyframes { stops, tracks }
}

fn supports_animation_declaration(name: &str, value: &str) -> bool {
    crate::style::supports_declaration(name, value)
        || (name == "background" && crate::style::parse_color(value).is_some())
}

#[cfg(test)]
fn extract_keyframes(css: &str) -> Vec<(String, Keyframes)> {
    let mut conditions = vec![ContainerConditionNode {
        parent: ContainerConditionId::NONE,
        alternatives: Vec::new(),
    }];
    let parsed = parse_stylesheet_for_viewport_preserving_containers(
        css,
        (1280.0, 720.0),
        CssMediaType::Screen,
        &mut conditions,
        ContainerConditionId::NONE,
    );
    parsed
        .into_iter()
        .filter_map(|rule| {
            let name = rule
                .selector
                .strip_prefix(KEYFRAMES_SELECTOR_PREFIX)
                .or_else(|| rule.selector.strip_prefix(WEBKIT_KEYFRAMES_SELECTOR_PREFIX))?;
            Some((name.to_string(), compile_keyframe_body(&rule.declarations)))
        })
        .collect()
}

fn animated_properties_for_declaration(name: &str) -> Vec<AnimatedProperty> {
    use AnimatedProperty::*;
    match name {
        "transform" => vec![Transform],
        "translate" => vec![Translate],
        "rotate" => vec![Rotate],
        "scale" => vec![Scale],
        "width" => vec![Width],
        "height" => vec![Height],
        "min-width" => vec![MinWidth],
        "min-height" => vec![MinHeight],
        "max-width" => vec![MaxWidth],
        "max-height" => vec![MaxHeight],
        "top" | "inset-block-start" => vec![Top],
        "right" | "inset-inline-end" => vec![Right],
        "bottom" | "inset-block-end" => vec![Bottom],
        "left" | "inset-inline-start" => vec![Left],
        "inset" => vec![Top, Right, Bottom, Left],
        "inset-inline" => vec![Left, Right],
        "inset-block" => vec![Top, Bottom],
        "margin" => vec![MarginTop, MarginRight, MarginBottom, MarginLeft],
        "margin-top" | "margin-block-start" => vec![MarginTop],
        "margin-right" | "margin-inline-end" => vec![MarginRight],
        "margin-bottom" | "margin-block-end" => vec![MarginBottom],
        "margin-left" | "margin-inline-start" => vec![MarginLeft],
        "margin-inline" => vec![MarginLeft, MarginRight],
        "margin-block" => vec![MarginTop, MarginBottom],
        "padding" => vec![PaddingTop, PaddingRight, PaddingBottom, PaddingLeft],
        "padding-top" | "padding-block-start" => vec![PaddingTop],
        "padding-right" | "padding-inline-end" => vec![PaddingRight],
        "padding-bottom" | "padding-block-end" => vec![PaddingBottom],
        "padding-left" | "padding-inline-start" => vec![PaddingLeft],
        "padding-inline" => vec![PaddingLeft, PaddingRight],
        "padding-block" => vec![PaddingTop, PaddingBottom],
        "gap" | "grid-gap" => vec![RowGap, ColumnGap],
        "row-gap" | "grid-row-gap" => vec![RowGap],
        "column-gap" | "grid-column-gap" | "-webkit-column-gap" => vec![ColumnGap],
        "flex-basis" => vec![FlexBasis],
        "opacity" => vec![Opacity],
        "color" | "-webkit-text-fill-color" => vec![Color],
        "background-color" => vec![BackgroundColor],
        "background" => vec![BackgroundColor, BackgroundPosition],
        "border-color" => vec![
            BorderTopColor,
            BorderRightColor,
            BorderBottomColor,
            BorderLeftColor,
        ],
        "border" => vec![
            BorderTopColor,
            BorderRightColor,
            BorderBottomColor,
            BorderLeftColor,
        ],
        "border-top" | "border-top-color" => vec![BorderTopColor],
        "border-right" | "border-right-color" => vec![BorderRightColor],
        "border-bottom" | "border-bottom-color" => vec![BorderBottomColor],
        "border-left" | "border-left-color" => vec![BorderLeftColor],
        "background-position" => vec![BackgroundPosition],
        "visibility" => vec![Visibility],
        _ => Vec::new(),
    }
}

fn parse_keyframe_offset(value: &str) -> Option<f32> {
    let value = value.trim().to_ascii_lowercase();
    match value.as_str() {
        "from" => Some(0.0),
        "to" => Some(1.0),
        _ => value
            .strip_suffix('%')?
            .trim()
            .parse::<f32>()
            .ok()
            .filter(|offset| offset.is_finite() && (0.0..=100.0).contains(offset))
            .map(|offset| offset / 100.0),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AnimationPhase {
    Before,
    Active,
    After,
}

#[derive(Clone, Debug)]
enum AnimatedLength {
    Auto,
    Dimension(crate::Dimension),
    Expression(String),
}

#[derive(Clone, Debug)]
enum AnimationValue {
    Length(AnimatedLength),
    Number(f32),
    Color([u8; 4]),
    Transform(Vec<crate::TransformOp>),
    Translate(crate::TransformLength, crate::TransformLength),
    Rotate(f32),
    Scale(f32, f32),
    BackgroundPosition(crate::BackgroundPosition),
    Visibility(bool),
}

fn sample_animation_properties(
    keyframes: &Keyframes,
    style: &mut LayoutStyle,
    timing: &crate::AnimationTiming,
    sample_time: crate::AnimationSampleTime,
    props: &HashMap<String, String>,
) {
    let Some(progress) = animation_directed_progress(timing, sample_time) else {
        return;
    };
    // Keep the base style immutable while resolving every property. Otherwise
    // one sampled property could accidentally become another track's implicit
    // endpoint, which is not how the animation cascade origin is defined.
    let underlying = style.clone();
    for (&property, track) in &keyframes.tracks {
        if let Some(value) = sample_property_track(property, track, &underlying, progress, props) {
            apply_animation_value(style, property, value);
        }
    }
}

fn sample_property_track(
    property: AnimatedProperty,
    track: &[PropertyTrackStop],
    underlying: &LayoutStyle,
    progress: f32,
    props: &HashMap<String, String>,
) -> Option<AnimationValue> {
    let underlying_value = animation_value_from_style(property, underlying)?;
    let mut resolved = Vec::<(f32, AnimationValue)>::with_capacity(track.len() + 2);
    for stop in track {
        let Some(value) = substitute_var_value(&stop.declaration.value, props, 0) else {
            continue;
        };
        if !supports_animation_declaration(&stop.declaration.name, &value) {
            continue;
        }
        let mut endpoint = underlying.clone();
        crate::style::apply_animation_property_value(&mut endpoint, &stop.declaration.name, &value);
        if let Some(value) = animation_value_from_style(property, &endpoint) {
            resolved.push((stop.offset, value));
        }
    }
    if resolved.is_empty() {
        return None;
    }
    if resolved.first().is_none_or(|stop| stop.0 > 0.0) {
        resolved.insert(0, (0.0, underlying_value.clone()));
    }
    if resolved.last().is_none_or(|stop| stop.0 < 1.0) {
        resolved.push((1.0, underlying_value));
    }
    if progress <= resolved[0].0 {
        return Some(resolved.remove(0).1);
    }
    for pair in resolved.windows(2) {
        let (from_offset, from_value) = (&pair[0].0, &pair[0].1);
        let (to_offset, to_value) = (&pair[1].0, &pair[1].1);
        if progress <= *to_offset {
            if progress == *to_offset || to_offset == from_offset {
                return Some(to_value.clone());
            }
            let position = (progress - *from_offset) / (*to_offset - *from_offset);
            return Some(interpolate_animation_value(from_value, to_value, position));
        }
    }
    resolved.last().map(|stop| stop.1.clone())
}

fn animation_value_from_style(
    property: AnimatedProperty,
    style: &LayoutStyle,
) -> Option<AnimationValue> {
    use AnimatedProperty::*;
    match property {
        Transform => Some(AnimationValue::Transform(style.transform_ops.clone())),
        Translate => {
            let (x, y) = style
                .individual_translate
                .unwrap_or((crate::Dimension::Px(0.0), crate::Dimension::Px(0.0)));
            Some(AnimationValue::Translate(
                crate::TransformLength {
                    value: x,
                    expression: style.individual_translate_expressions[0].clone(),
                },
                crate::TransformLength {
                    value: y,
                    expression: style.individual_translate_expressions[1].clone(),
                },
            ))
        }
        Rotate => Some(AnimationValue::Rotate(
            style.individual_rotate.unwrap_or(0.0),
        )),
        Scale => {
            let (x, y) = style.individual_scale.unwrap_or((1.0, 1.0));
            Some(AnimationValue::Scale(x, y))
        }
        Width | Height | MinWidth | MinHeight | MaxWidth | MaxHeight => {
            let index = match property {
                Width => 0,
                Height => 1,
                MinWidth => 2,
                MinHeight => 3,
                MaxWidth => 4,
                MaxHeight => 5,
                _ => unreachable!(),
            };
            if matches!(property, Width) && style.width_fit_content {
                return None;
            }
            let dimension = match property {
                Width => style.width,
                Height => style.height,
                MinWidth => style.min_width,
                MinHeight => style.min_height,
                MaxWidth => style.max_width,
                MaxHeight => style.max_height,
                _ => unreachable!(),
            };
            Some(AnimationValue::Length(length_with_expression(
                dimension,
                &style.size_expressions[index],
            )))
        }
        Top | Right | Bottom | Left => {
            let index = physical_side_index(property);
            Some(AnimationValue::Length(
                match &style.inset_expressions[index] {
                    Some(expression) => AnimatedLength::Expression(expression.clone()),
                    None => style.inset[index]
                        .map(AnimatedLength::Dimension)
                        .unwrap_or(AnimatedLength::Auto),
                },
            ))
        }
        MarginTop | MarginRight | MarginBottom | MarginLeft => {
            let index = physical_side_index(property);
            Some(AnimationValue::Length(margin_length(style, index)))
        }
        PaddingTop | PaddingRight | PaddingBottom | PaddingLeft => {
            let index = physical_side_index(property);
            Some(AnimationValue::Length(padding_length(style, index)))
        }
        RowGap | ColumnGap => {
            let (value, expression) = if property == RowGap {
                (style.row_gap, &style.row_gap_expression)
            } else {
                (style.column_gap, &style.column_gap_expression)
            };
            Some(AnimationValue::Length(match expression {
                Some(expression) => AnimatedLength::Expression(expression.clone()),
                None => value
                    .map(|value| AnimatedLength::Dimension(crate::Dimension::Px(value)))
                    .unwrap_or(AnimatedLength::Auto),
            }))
        }
        FlexBasis => Some(AnimationValue::Length(AnimatedLength::Dimension(
            style.flex_basis,
        ))),
        Opacity => Some(AnimationValue::Number(
            style.opacity.unwrap_or(1.0).clamp(0.0, 1.0),
        )),
        Color => Some(AnimationValue::Color(style.color.unwrap_or([0, 0, 0, 255]))),
        BackgroundColor => Some(AnimationValue::Color(
            style.background_color.unwrap_or([0, 0, 0, 0]),
        )),
        BorderTopColor | BorderRightColor | BorderBottomColor | BorderLeftColor => {
            let colors = style.border_model.colors;
            let color = match property {
                BorderTopColor => colors.top,
                BorderRightColor => colors.right,
                BorderBottomColor => colors.bottom,
                BorderLeftColor => colors.left,
                _ => unreachable!(),
            }
            .or(style.color)
            .unwrap_or([0, 0, 0, 255]);
            Some(AnimationValue::Color(color))
        }
        BackgroundPosition => Some(AnimationValue::BackgroundPosition(
            style.background_position,
        )),
        Visibility => Some(AnimationValue::Visibility(
            !style.visibility_hidden.unwrap_or(false),
        )),
    }
}

fn apply_animation_value(
    style: &mut LayoutStyle,
    property: AnimatedProperty,
    value: AnimationValue,
) {
    use AnimatedProperty::*;
    match (property, value) {
        (Transform, AnimationValue::Transform(operations)) => {
            style.transform_ops = operations;
            set_animation_containing_block_trigger(
                style,
                crate::CB_TRIGGER_TRANSFORM,
                !style.transform_ops.is_empty(),
            );
        }
        (Translate, AnimationValue::Translate(x, y)) => {
            style.individual_translate = Some((x.value, y.value));
            style.individual_translate_expressions = [x.expression, y.expression];
            set_animation_containing_block_trigger(style, crate::CB_TRIGGER_TRANSLATE, true);
        }
        (Rotate, AnimationValue::Rotate(value)) => {
            style.individual_rotate = Some(value);
            set_animation_containing_block_trigger(style, crate::CB_TRIGGER_ROTATE, true);
        }
        (Scale, AnimationValue::Scale(x, y)) => {
            style.individual_scale = Some((x, y));
            set_animation_containing_block_trigger(style, crate::CB_TRIGGER_SCALE, true);
        }
        (
            Width | Height | MinWidth | MinHeight | MaxWidth | MaxHeight,
            AnimationValue::Length(value),
        ) => {
            set_size_animation_value(style, property, value);
        }
        (Top | Right | Bottom | Left, AnimationValue::Length(value)) => {
            let index = physical_side_index(property);
            set_inset_animation_value(style, index, value);
        }
        (MarginTop | MarginRight | MarginBottom | MarginLeft, AnimationValue::Length(value)) => {
            let index = physical_side_index(property);
            set_margin_animation_value(style, index, value);
        }
        (
            PaddingTop | PaddingRight | PaddingBottom | PaddingLeft,
            AnimationValue::Length(value),
        ) => {
            let index = physical_side_index(property);
            set_padding_animation_value(style, index, value);
        }
        (RowGap | ColumnGap, AnimationValue::Length(value)) => {
            set_gap_animation_value(style, property == RowGap, value);
        }
        (FlexBasis, AnimationValue::Length(AnimatedLength::Dimension(value))) => {
            style.flex_basis = value;
        }
        (Opacity, AnimationValue::Number(value)) => {
            style.opacity = Some(value.clamp(0.0, 1.0));
        }
        (Color, AnimationValue::Color(value)) => style.color = Some(value),
        (BackgroundColor, AnimationValue::Color(value)) => style.background_color = Some(value),
        (BorderTopColor, AnimationValue::Color(value)) => {
            style.border_model.colors.top = Some(value)
        }
        (BorderRightColor, AnimationValue::Color(value)) => {
            style.border_model.colors.right = Some(value)
        }
        (BorderBottomColor, AnimationValue::Color(value)) => {
            style.border_model.colors.bottom = Some(value)
        }
        (BorderLeftColor, AnimationValue::Color(value)) => {
            style.border_model.colors.left = Some(value)
        }
        (BackgroundPosition, AnimationValue::BackgroundPosition(value)) => {
            style.background_position = value
        }
        (Visibility, AnimationValue::Visibility(visible)) => {
            style.visibility_hidden = Some(!visible)
        }
        _ => return,
    }
    let colors = style.border_model.colors;
    style.border_color =
        (colors.top == colors.right && colors.top == colors.bottom && colors.top == colors.left)
            .then_some(colors.top)
            .flatten();
}

fn interpolate_animation_value(
    from: &AnimationValue,
    to: &AnimationValue,
    position: f32,
) -> AnimationValue {
    match (from, to) {
        (AnimationValue::Length(from), AnimationValue::Length(to)) => {
            AnimationValue::Length(interpolate_animated_length(from, to, position))
        }
        (AnimationValue::Number(from), AnimationValue::Number(to)) => {
            AnimationValue::Number(from + (to - from) * position)
        }
        (AnimationValue::Color(from), AnimationValue::Color(to)) => {
            AnimationValue::Color(interpolate_color(*from, *to, position))
        }
        (AnimationValue::Transform(from), AnimationValue::Transform(to)) => {
            AnimationValue::Transform(interpolate_transform_list(from, to, position))
        }
        (AnimationValue::Translate(from_x, from_y), AnimationValue::Translate(to_x, to_y)) => {
            let x = interpolate_transform_length(from_x, to_x, position);
            let y = interpolate_transform_length(from_y, to_y, position);
            match (x, y) {
                (Some(x), Some(y)) => AnimationValue::Translate(x, y),
                _ if position < 0.5 => from.clone(),
                _ => to.clone(),
            }
        }
        (AnimationValue::Rotate(from), AnimationValue::Rotate(to)) => {
            AnimationValue::Rotate(from + (to - from) * position)
        }
        (AnimationValue::Scale(from_x, from_y), AnimationValue::Scale(to_x, to_y)) => {
            AnimationValue::Scale(
                from_x + (to_x - from_x) * position,
                from_y + (to_y - from_y) * position,
            )
        }
        (AnimationValue::BackgroundPosition(from), AnimationValue::BackgroundPosition(to)) => {
            AnimationValue::BackgroundPosition(from.interpolate(*to, position))
        }
        (AnimationValue::Visibility(from), AnimationValue::Visibility(to)) => {
            // visibility has a special discrete interpolation: if either end
            // is visible, every interior value is visible.
            AnimationValue::Visibility(if *from || *to {
                position > 0.0 && position < 1.0 || if position <= 0.0 { *from } else { *to }
            } else {
                false
            })
        }
        _ if position < 0.5 => from.clone(),
        _ => to.clone(),
    }
}

fn physical_side_index(property: AnimatedProperty) -> usize {
    use AnimatedProperty::*;
    match property {
        Top | MarginTop | PaddingTop | BorderTopColor => 0,
        Right | MarginRight | PaddingRight | BorderRightColor => 1,
        Bottom | MarginBottom | PaddingBottom | BorderBottomColor => 2,
        Left | MarginLeft | PaddingLeft | BorderLeftColor => 3,
        _ => unreachable!("property has no physical side"),
    }
}

fn edge_value(edges: crate::Edges, index: usize) -> f32 {
    match index {
        0 => edges.top,
        1 => edges.right,
        2 => edges.bottom,
        3 => edges.left,
        _ => unreachable!(),
    }
}

fn edge_value_mut(edges: &mut crate::Edges, index: usize) -> &mut f32 {
    match index {
        0 => &mut edges.top,
        1 => &mut edges.right,
        2 => &mut edges.bottom,
        3 => &mut edges.left,
        _ => unreachable!(),
    }
}

fn length_with_expression(
    dimension: crate::Dimension,
    expression: &Option<String>,
) -> AnimatedLength {
    expression
        .as_ref()
        .map(|expression| AnimatedLength::Expression(expression.clone()))
        .unwrap_or(AnimatedLength::Dimension(dimension))
}

fn margin_length(style: &LayoutStyle, index: usize) -> AnimatedLength {
    if style.margin_auto[index] {
        AnimatedLength::Auto
    } else if let Some(expression) = &style.margin_expressions[index] {
        AnimatedLength::Expression(expression.clone())
    } else if let Some(percentage) = style.margin_percent[index] {
        AnimatedLength::Dimension(crate::Dimension::Percent(percentage))
    } else if let Some(relative) = style.margin_relative[index] {
        AnimatedLength::Dimension(relative)
    } else {
        AnimatedLength::Dimension(crate::Dimension::Px(edge_value(style.margin, index)))
    }
}

fn padding_length(style: &LayoutStyle, index: usize) -> AnimatedLength {
    if let Some(expression) = &style.padding_expressions[index] {
        AnimatedLength::Expression(expression.clone())
    } else if let Some(percentage) = style.padding_percent[index] {
        AnimatedLength::Dimension(crate::Dimension::Percent(percentage))
    } else if let Some(relative) = style.padding_relative[index] {
        AnimatedLength::Dimension(relative)
    } else {
        AnimatedLength::Dimension(crate::Dimension::Px(edge_value(style.padding, index)))
    }
}

fn set_size_animation_value(
    style: &mut LayoutStyle,
    property: AnimatedProperty,
    value: AnimatedLength,
) {
    use AnimatedProperty::*;
    let index = match property {
        Width => 0,
        Height => 1,
        MinWidth => 2,
        MinHeight => 3,
        MaxWidth => 4,
        MaxHeight => 5,
        _ => unreachable!(),
    };
    let (dimension, expression) = match value {
        AnimatedLength::Auto => (crate::Dimension::Auto, None),
        AnimatedLength::Dimension(value) => (value, None),
        AnimatedLength::Expression(value) => (crate::Dimension::Auto, Some(value)),
    };
    style.size_expressions[index] = expression;
    match property {
        Width => {
            style.width = dimension;
            style.width_set = true;
            style.width_fit_content = false;
        }
        Height => {
            style.height = dimension;
            style.height_set = true;
        }
        MinWidth => style.min_width = dimension,
        MinHeight => style.min_height = dimension,
        MaxWidth => style.max_width = dimension,
        MaxHeight => style.max_height = dimension,
        _ => unreachable!(),
    }
}

fn set_inset_animation_value(style: &mut LayoutStyle, index: usize, value: AnimatedLength) {
    match value {
        AnimatedLength::Auto => {
            style.inset[index] = None;
            style.inset_expressions[index] = None;
        }
        AnimatedLength::Dimension(value) => {
            style.inset[index] = Some(value);
            style.inset_expressions[index] = None;
        }
        AnimatedLength::Expression(value) => {
            style.inset[index] = None;
            style.inset_expressions[index] = Some(value);
        }
    }
}

fn set_margin_animation_value(style: &mut LayoutStyle, index: usize, value: AnimatedLength) {
    style.margin_auto[index] = false;
    style.margin_percent[index] = None;
    style.margin_relative[index] = None;
    style.margin_expressions[index] = None;
    *edge_value_mut(&mut style.margin, index) = 0.0;
    match value {
        AnimatedLength::Auto => style.margin_auto[index] = true,
        AnimatedLength::Dimension(crate::Dimension::Px(value)) => {
            *edge_value_mut(&mut style.margin, index) = value
        }
        AnimatedLength::Dimension(crate::Dimension::Percent(value)) => {
            style.margin_percent[index] = Some(value)
        }
        AnimatedLength::Dimension(crate::Dimension::Auto) => style.margin_auto[index] = true,
        AnimatedLength::Dimension(value) => style.margin_relative[index] = Some(value),
        AnimatedLength::Expression(value) => style.margin_expressions[index] = Some(value),
    }
}

fn set_padding_animation_value(style: &mut LayoutStyle, index: usize, value: AnimatedLength) {
    style.padding_percent[index] = None;
    style.padding_relative[index] = None;
    style.padding_expressions[index] = None;
    *edge_value_mut(&mut style.padding, index) = 0.0;
    match value {
        AnimatedLength::Auto | AnimatedLength::Dimension(crate::Dimension::Auto) => {}
        AnimatedLength::Dimension(crate::Dimension::Px(value)) => {
            *edge_value_mut(&mut style.padding, index) = value
        }
        AnimatedLength::Dimension(crate::Dimension::Percent(value)) => {
            style.padding_percent[index] = Some(value)
        }
        AnimatedLength::Dimension(value) => style.padding_relative[index] = Some(value),
        AnimatedLength::Expression(value) => style.padding_expressions[index] = Some(value),
    }
}

fn set_gap_animation_value(style: &mut LayoutStyle, row: bool, value: AnimatedLength) {
    let (slot, expression) = if row {
        (&mut style.row_gap, &mut style.row_gap_expression)
    } else {
        (&mut style.column_gap, &mut style.column_gap_expression)
    };
    match value {
        AnimatedLength::Auto | AnimatedLength::Dimension(crate::Dimension::Auto) => {
            *slot = None;
            *expression = None;
        }
        AnimatedLength::Dimension(crate::Dimension::Px(value)) => {
            *slot = Some(value);
            *expression = None;
        }
        AnimatedLength::Dimension(value) => {
            *slot = None;
            *expression = Some(dimension_to_css(value));
        }
        AnimatedLength::Expression(value) => {
            *slot = None;
            *expression = Some(value);
        }
    }
}

fn dimension_to_css(value: crate::Dimension) -> String {
    match value {
        crate::Dimension::Auto => "auto".to_string(),
        crate::Dimension::Px(value) => format!("{value}px"),
        crate::Dimension::Percent(value) => format!("{}%", value * 100.0),
        crate::Dimension::Em(value) => format!("{value}em"),
        crate::Dimension::Ex(value) => format!("{value}ex"),
        crate::Dimension::Rem(value) => format!("{value}rem"),
        crate::Dimension::Vw(value) => format!("{value}vw"),
        crate::Dimension::Vh(value) => format!("{value}vh"),
        crate::Dimension::Vmin(value) => format!("{value}vmin"),
        crate::Dimension::Vmax(value) => format!("{value}vmax"),
    }
}

fn interpolate_animated_length(
    from: &AnimatedLength,
    to: &AnimatedLength,
    position: f32,
) -> AnimatedLength {
    match (from, to) {
        (AnimatedLength::Dimension(from), AnimatedLength::Dimension(to)) => {
            interpolate_dimension(*from, *to, position)
                .map(AnimatedLength::Dimension)
                .unwrap_or_else(|| {
                    AnimatedLength::Dimension(if position < 0.5 { *from } else { *to })
                })
        }
        (AnimatedLength::Auto, AnimatedLength::Auto) => AnimatedLength::Auto,
        (AnimatedLength::Expression(from), AnimatedLength::Expression(to)) if from == to => {
            AnimatedLength::Expression(from.clone())
        }
        _ if position < 0.5 => from.clone(),
        _ => to.clone(),
    }
}

fn interpolate_dimension(
    from: crate::Dimension,
    to: crate::Dimension,
    position: f32,
) -> Option<crate::Dimension> {
    use crate::Dimension::*;
    let lerp = |from: f32, to: f32| from + (to - from) * position;
    Some(match (from, to) {
        (Auto, Auto) => Auto,
        (Px(from), Px(to)) => Px(lerp(from, to)),
        (Percent(from), Percent(to)) => Percent(lerp(from, to)),
        (Em(from), Em(to)) => Em(lerp(from, to)),
        (Ex(from), Ex(to)) => Ex(lerp(from, to)),
        (Rem(from), Rem(to)) => Rem(lerp(from, to)),
        (Vw(from), Vw(to)) => Vw(lerp(from, to)),
        (Vh(from), Vh(to)) => Vh(lerp(from, to)),
        (Vmin(from), Vmin(to)) => Vmin(lerp(from, to)),
        (Vmax(from), Vmax(to)) => Vmax(lerp(from, to)),
        _ => return None,
    })
}

fn interpolate_color(from: [u8; 4], to: [u8; 4], position: f32) -> [u8; 4] {
    let channel = |from: u8, to: u8| {
        (f32::from(from) + (f32::from(to) - f32::from(from)) * position)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    [
        channel(from[0], to[0]),
        channel(from[1], to[1]),
        channel(from[2], to[2]),
        channel(from[3], to[3]),
    ]
}

fn interpolate_transform_length(
    from: &crate::TransformLength,
    to: &crate::TransformLength,
    position: f32,
) -> Option<crate::TransformLength> {
    match (&from.expression, &to.expression) {
        (None, None) => Some(crate::TransformLength {
            value: interpolate_dimension(from.value, to.value, position)?,
            expression: None,
        }),
        (Some(from_expression), Some(to_expression)) if from_expression == to_expression => {
            Some(crate::TransformLength {
                value: interpolate_dimension(from.value, to.value, position).unwrap_or(from.value),
                expression: Some(from_expression.clone()),
            })
        }
        _ => None,
    }
}

fn interpolate_transform_list(
    from: &[crate::TransformOp],
    to: &[crate::TransformOp],
    position: f32,
) -> Vec<crate::TransformOp> {
    let from_list = if from.is_empty() && !to.is_empty() {
        to.iter()
            .map(identity_transform_operation)
            .collect::<Vec<_>>()
    } else {
        from.to_vec()
    };
    let to_list = if to.is_empty() && !from.is_empty() {
        from.iter()
            .map(identity_transform_operation)
            .collect::<Vec<_>>()
    } else {
        to.to_vec()
    };
    if from_list.len() != to_list.len() {
        return if position < 0.5 { from_list } else { to_list };
    }
    let mut result = Vec::with_capacity(from_list.len());
    for (from, to) in from_list.iter().zip(&to_list) {
        let Some(operation) = interpolate_transform_operation(from, to, position) else {
            return if position < 0.5 { from_list } else { to_list };
        };
        result.push(operation);
    }
    result
}

fn identity_transform_operation(operation: &crate::TransformOp) -> crate::TransformOp {
    match operation {
        crate::TransformOp::Translate(x, y) => {
            crate::TransformOp::Translate(zero_transform_length(x), zero_transform_length(y))
        }
        crate::TransformOp::Scale(_, _) => crate::TransformOp::Scale(1.0, 1.0),
        crate::TransformOp::Rotate(_) => crate::TransformOp::Rotate(0.0),
        crate::TransformOp::Skew(_, _) => crate::TransformOp::Skew(0.0, 0.0),
        crate::TransformOp::Matrix(_) => crate::TransformOp::Matrix(crate::Affine2::IDENTITY),
    }
}

fn zero_transform_length(value: &crate::TransformLength) -> crate::TransformLength {
    crate::TransformLength {
        value: match value.value {
            crate::Dimension::Percent(_) => crate::Dimension::Percent(0.0),
            crate::Dimension::Em(_) => crate::Dimension::Em(0.0),
            crate::Dimension::Ex(_) => crate::Dimension::Ex(0.0),
            crate::Dimension::Rem(_) => crate::Dimension::Rem(0.0),
            crate::Dimension::Vw(_) => crate::Dimension::Vw(0.0),
            crate::Dimension::Vh(_) => crate::Dimension::Vh(0.0),
            crate::Dimension::Vmin(_) => crate::Dimension::Vmin(0.0),
            crate::Dimension::Vmax(_) => crate::Dimension::Vmax(0.0),
            _ => crate::Dimension::Px(0.0),
        },
        expression: value.expression.as_ref().map(|_| "0px".to_string()),
    }
}

fn interpolate_transform_operation(
    from: &crate::TransformOp,
    to: &crate::TransformOp,
    position: f32,
) -> Option<crate::TransformOp> {
    let lerp = |from: f32, to: f32| from + (to - from) * position;
    Some(match (from, to) {
        (
            crate::TransformOp::Translate(from_x, from_y),
            crate::TransformOp::Translate(to_x, to_y),
        ) => crate::TransformOp::Translate(
            interpolate_transform_length(from_x, to_x, position)?,
            interpolate_transform_length(from_y, to_y, position)?,
        ),
        (crate::TransformOp::Scale(from_x, from_y), crate::TransformOp::Scale(to_x, to_y)) => {
            crate::TransformOp::Scale(lerp(*from_x, *to_x), lerp(*from_y, *to_y))
        }
        (crate::TransformOp::Rotate(from), crate::TransformOp::Rotate(to)) => {
            crate::TransformOp::Rotate(lerp(*from, *to))
        }
        (crate::TransformOp::Skew(from_x, from_y), crate::TransformOp::Skew(to_x, to_y)) => {
            crate::TransformOp::Skew(lerp(*from_x, *to_x), lerp(*from_y, *to_y))
        }
        (crate::TransformOp::Matrix(from), crate::TransformOp::Matrix(to)) => {
            crate::TransformOp::Matrix(crate::Affine2 {
                a: lerp(from.a, to.a),
                b: lerp(from.b, to.b),
                c: lerp(from.c, to.c),
                d: lerp(from.d, to.d),
                e: lerp(from.e, to.e),
                f: lerp(from.f, to.f),
            })
        }
        _ => return None,
    })
}

fn set_animation_containing_block_trigger(style: &mut LayoutStyle, trigger: u16, enabled: bool) {
    if enabled {
        style.containing_block_triggers |= trigger;
    } else {
        style.containing_block_triggers &= !trigger;
    }
}

fn animation_directed_progress(
    timing: &crate::AnimationTiming,
    sample_time: crate::AnimationSampleTime,
) -> Option<f32> {
    let duration = timing.duration_ms.max(0.0);
    let iterations = timing.iteration_count.max(0.0);
    let active_duration = if duration == 0.0 || iterations == 0.0 {
        0.0
    } else {
        duration * iterations
    };
    // The timeline owns pause/resume hold time. Keeping that state outside the
    // sampler lets a running animation freeze at its current progress and
    // later resume without manufacturing a new instance.
    let local_time = sample_time.milliseconds;
    if !local_time.is_finite() {
        return None;
    }
    let end_time = (timing.delay_ms + active_duration).max(0.0);
    let before_boundary = timing.delay_ms.clamp(0.0, end_time);
    let after_boundary = (timing.delay_ms + active_duration).min(end_time).max(0.0);
    let phase = if local_time < before_boundary {
        AnimationPhase::Before
    } else if local_time >= after_boundary {
        AnimationPhase::After
    } else {
        AnimationPhase::Active
    };
    let active_time = match phase {
        AnimationPhase::Before => {
            if !matches!(
                timing.fill_mode,
                crate::AnimationFillMode::Backwards | crate::AnimationFillMode::Both
            ) {
                return None;
            }
            (local_time - timing.delay_ms).max(0.0)
        }
        AnimationPhase::Active => local_time - timing.delay_ms,
        AnimationPhase::After => {
            if !matches!(
                timing.fill_mode,
                crate::AnimationFillMode::Forwards | crate::AnimationFillMode::Both
            ) {
                return None;
            }
            (local_time - timing.delay_ms).clamp(0.0, active_duration)
        }
    };

    let overall_progress = if duration == 0.0 {
        if phase == AnimationPhase::Before {
            0.0
        } else {
            iterations
        }
    } else {
        active_time / duration
    };
    if !overall_progress.is_finite() {
        return Some(match timing.direction {
            crate::AnimationDirection::Reverse | crate::AnimationDirection::AlternateReverse => 1.0,
            _ => 0.0,
        });
    }
    let mut current_iteration = overall_progress.floor().max(0.0);
    let mut simple_progress = overall_progress.rem_euclid(1.0);
    if phase == AnimationPhase::After && iterations > 0.0 && simple_progress == 0.0 {
        simple_progress = 1.0;
        current_iteration = (current_iteration - 1.0).max(0.0);
    }
    let reverse = match timing.direction {
        crate::AnimationDirection::Normal => false,
        crate::AnimationDirection::Reverse => true,
        crate::AnimationDirection::Alternate => current_iteration.rem_euclid(2.0) >= 1.0,
        crate::AnimationDirection::AlternateReverse => current_iteration.rem_euclid(2.0) < 1.0,
    };
    Some(if reverse {
        1.0 - simple_progress
    } else {
        simple_progress
    })
}

fn normalized_keyframe_offsets(stops: &[KeyframeStop]) -> Vec<(f32, &KeyframeStop)> {
    if stops.is_empty() {
        return Vec::new();
    }
    let mut offsets = stops.iter().map(|stop| stop.offset).collect::<Vec<_>>();
    if offsets.len() == 1 {
        offsets[0] = Some(offsets[0].unwrap_or(1.0));
    } else {
        if offsets[0].is_none() {
            offsets[0] = Some(0.0);
        }
        let last = offsets.len() - 1;
        if offsets[last].is_none() {
            offsets[last] = Some(1.0);
        }
    }
    let mut index = 0usize;
    while index < offsets.len() {
        if offsets[index].is_some() {
            index += 1;
            continue;
        }
        let start = index - 1;
        let mut end = index + 1;
        while offsets[end].is_none() {
            end += 1;
        }
        let from = offsets[start].unwrap();
        let to = offsets[end].unwrap();
        let span = (end - start) as f32;
        for missing in index..end {
            offsets[missing] = Some(from + (to - from) * (missing - start) as f32 / span);
        }
        index = end + 1;
    }
    stops
        .iter()
        .zip(offsets)
        .map(|(stop, offset)| (offset.unwrap(), stop))
        .collect()
}

/// Resolve variables one declaration at a time. An invalid variable poisons
/// its entire declaration at computed-value time, but must not erase unrelated
/// declarations in the same rule.
fn substitute_declarations<'a>(
    css: &'a str,
    props: &HashMap<String, String>,
    has_var: bool,
) -> Cow<'a, str> {
    // Partitioned rule streams are already normalized declaration blocks. If
    // no value contains var(), applying them directly is both equivalent and
    // avoids a String plus one split/serialize pass for every matched rule.
    if !has_var {
        return Cow::Borrowed(css);
    }
    let mut expanded = String::new();
    for declaration in crate::style::split_declarations(css) {
        let Some((name, value)) = declaration.split_once(':') else {
            continue;
        };
        let Some(value) = substitute_var_value(value.trim(), props, 0) else {
            continue;
        };
        expanded.push_str(name.trim());
        expanded.push(':');
        expanded.push_str(&value);
        expanded.push(';');
    }
    Cow::Owned(expanded)
}

/// Substitute every `var(--name, fallback?)` in one property value. `None`
/// represents CSS's guaranteed-invalid value. Crucially, invalidity propagates
/// through an intermediate custom property so an outer var() can use its own
/// fallback (`--toggle:var(--missing) dark; color:var(--toggle,light)`).
pub(crate) fn substitute_var_value(
    input: &str,
    props: &HashMap<String, String>,
    depth: u8,
) -> Option<String> {
    if depth > 16 {
        return None;
    }
    if !input.contains("var(") {
        return Some(input.to_string());
    }
    let mut out = String::new();
    let mut rest = input;
    while let Some(pos) = rest.find("var(") {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 4..];
        // Matching close paren, respecting nesting.
        let mut d = 1i32;
        let mut end = None;
        for (i, ch) in after.char_indices() {
            match ch {
                '(' => d += 1,
                ')' => {
                    d -= 1;
                    if d == 0 {
                        end = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = end else {
            return None;
        };
        let inner = &after[..end];
        let (name, fallback) = match inner.split_once(',') {
            Some((n, f)) => (n.trim(), Some(f.trim())),
            None => (inner.trim(), None),
        };
        let resolved = props
            .get(name)
            .and_then(|value| substitute_var_value(value, props, depth + 1));
        let replacement = match resolved {
            Some(value) => value,
            None => {
                let fallback = fallback?;
                substitute_var_value(fallback, props, depth + 1)?
            }
        };
        // `var()` substitutes a token sequence, not source text. Insert a
        // separator only where reparsing would merge boundary tokens:
        // `2px` + `solid` must not become the dimension `2pxsolid`, and `10`
        // + `%` must not become a percentage. Do not add unconditional
        // whitespace: it is significant around calc()'s `+` and `-`.
        if out
            .chars()
            .next_back()
            .zip(replacement.chars().next())
            .is_some_and(|(left, right)| css_substitution_boundary_merges(left, right))
        {
            out.push(' ');
        }
        out.push_str(&replacement);
        if replacement
            .chars()
            .next_back()
            .zip(after[end + 1..].chars().next())
            .is_some_and(|(left, right)| css_substitution_boundary_merges(left, right))
        {
            out.push(' ');
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Some(out)
}

fn css_substitution_boundary_merges(left: char, right: char) -> bool {
    let name = |ch: char| {
        ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch == '\\' || !ch.is_ascii()
    };
    if left.is_ascii_digit() && right == '-' {
        // A number followed by `-` remains two tokens. Separating these would
        // incorrectly make `calc(var(--n)- 1px)` satisfy calc's whitespace
        // requirement.
        return false;
    }
    (name(left) && name(right))
        || ((left.is_ascii_digit() || left == '.') && (right == '.' || right == '%' || name(right)))
        || (matches!(left, '#' | '@') && name(right))
        || (!left.is_ascii_digit() && name(left) && right == '(')
        || (left == '+' && (right.is_ascii_digit() || right == '.'))
        || (left == '/' && right == '*')
}

/// If `selector`'s rightmost compound is the pseudo-element `which`
/// (`"before"` or `"after"`, matching either the modern `::` or legacy `:`
/// form), return everything before it, trimmed. Only a trailing
/// pseudo-element is handled: `li::before` strips to `li`, but a selector
/// that uses `::before` as anything other than the final component (not
/// valid CSS in the first place) is left alone.
fn strip_pseudo_element<'a>(selector: &'a str, which: &str) -> Option<&'a str> {
    for prefix in ["::", ":"] {
        let suffix = format!("{prefix}{which}");
        if let Some(base) = selector.strip_suffix(&suffix) {
            if !base.is_empty() {
                return Some(base.trim());
            }
        }
    }
    None
}

/// Return the final valid `content` declaration in a declaration list.
///
/// The outer option says whether a declaration was found; the inner option is
/// the generated text (`none`/`normal` suppress the pseudo). Along with quoted
/// strings, support the common `attr(name)` form used by component-library
/// buttons and badges. The attribute is resolved against the originating
/// element, as CSS generated content requires.
fn extract_content(
    decls: &str,
    tree: &DomTree,
    nid: NodeId,
) -> Option<Option<Vec<crate::GeneratedContentItem>>> {
    let mut result = None;
    for raw in crate::style::split_declarations(decls) {
        let Some((name, value)) = raw.split_once(':') else {
            continue;
        };
        if !name.trim().eq_ignore_ascii_case("content") {
            continue;
        }
        let value = value.trim();
        if value.eq_ignore_ascii_case("none") || value.eq_ignore_ascii_case("normal") {
            result = Some(None);
            continue;
        }
        let parsed = parse_generated_content_items(value, tree, nid);
        if let Some(parsed) = parsed {
            result = Some(Some(parsed));
        } else if value
            .trim_start()
            .get(..4)
            .map_or(false, |prefix| prefix.eq_ignore_ascii_case("url("))
        {
            // An image-valued content declaration supersedes any earlier
            // string declaration. The image itself is retained on
            // LayoutStyle::content_image by apply_declarations; clearing the
            // text here keeps the two views of the winning declaration in
            // sync and, importantly, keeps the pseudo alive.
            result = Some(None);
        }
    }
    result
}

fn generated_content_with_zero_counters(items: &[crate::GeneratedContentItem]) -> String {
    let mut text = String::new();
    for item in items {
        match item {
            crate::GeneratedContentItem::Text(value) => text.push_str(value),
            crate::GeneratedContentItem::Counter { style, .. } => {
                text.push_str(&format_counter_value(0, *style));
            }
            crate::GeneratedContentItem::Counters {
                style,
                separator: _,
                ..
            } => text.push_str(&format_counter_value(0, *style)),
        }
    }
    text
}

fn parse_generated_content_items(
    value: &str,
    tree: &DomTree,
    nid: NodeId,
) -> Option<Vec<crate::GeneratedContentItem>> {
    let mut items = Vec::new();
    let mut rest = value.trim();
    while !rest.is_empty() {
        rest = rest.trim_start();
        let first = rest.chars().next()?;
        if matches!(first, '"' | '\'') {
            let (raw, tail) = take_css_quoted(rest)?;
            items.push(crate::GeneratedContentItem::Text(unescape_css_string(raw)));
            rest = tail;
            continue;
        }

        let name_end = rest
            .char_indices()
            .find_map(|(index, ch)| (!is_css_ident_char(ch)).then_some(index))
            .unwrap_or(rest.len());
        let name = &rest[..name_end];
        let after_name = rest[name_end..].trim_start();
        if !after_name.starts_with('(') {
            // Quote-control keywords are valid generated-content items, but
            // they do not contribute text in the current renderer.
            if matches!(
                name.to_ascii_lowercase().as_str(),
                "open-quote" | "close-quote" | "no-open-quote" | "no-close-quote"
            ) {
                rest = after_name;
                continue;
            }
            return None;
        }
        let (arguments, tail) = take_css_function_arguments(after_name)?;
        match name.to_ascii_lowercase().as_str() {
            "attr" => {
                let attribute = arguments
                    .split_whitespace()
                    .next()
                    .filter(|name| !name.is_empty())?;
                let value = tree
                    .get_node(nid)
                    .and_then(|node| node.get_attribute(attribute).map(str::to_owned))
                    .unwrap_or_default();
                items.push(crate::GeneratedContentItem::Text(value));
            }
            "counter" => {
                let arguments = split_function_arguments(arguments);
                let name = arguments.first()?.trim();
                if !valid_generated_counter_name(name) || arguments.len() > 2 {
                    return None;
                }
                let style = match arguments.get(1) {
                    Some(style) => parse_generated_counter_style(style.trim())?,
                    None => crate::GeneratedCounterStyle::default(),
                };
                items.push(crate::GeneratedContentItem::Counter {
                    name: name.to_string(),
                    style,
                });
            }
            "counters" => {
                let arguments = split_function_arguments(arguments);
                if !(2..=3).contains(&arguments.len()) {
                    return None;
                }
                let name = arguments[0].trim();
                if !valid_generated_counter_name(name) {
                    return None;
                }
                let (separator, separator_tail) = take_css_quoted(arguments[1].trim())?;
                if !separator_tail.trim().is_empty() {
                    return None;
                }
                let style = match arguments.get(2) {
                    Some(style) => parse_generated_counter_style(style.trim())?,
                    None => crate::GeneratedCounterStyle::default(),
                };
                items.push(crate::GeneratedContentItem::Counters {
                    name: name.to_string(),
                    separator: unescape_css_string(separator),
                    style,
                });
            }
            _ => return None,
        }
        rest = tail;
    }
    (!items.is_empty()).then_some(items)
}

fn is_css_ident_char(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '-' | '_' | '\\')
}

fn take_css_quoted(value: &str) -> Option<(&str, &str)> {
    let quote = value.chars().next().filter(|ch| matches!(ch, '"' | '\''))?;
    let mut escaped = false;
    for (offset, ch) in value[quote.len_utf8()..].char_indices() {
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == quote {
            let end = quote.len_utf8() + offset;
            return Some((
                &value[quote.len_utf8()..end],
                &value[end + quote.len_utf8()..],
            ));
        }
    }
    None
}

fn take_css_function_arguments(value: &str) -> Option<(&str, &str)> {
    let mut depth = 0usize;
    let mut quote = None;
    let mut escaped = false;
    for (offset, ch) in value.char_indices() {
        if let Some(open) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == open {
                quote = None;
            }
            continue;
        }
        match ch {
            '"' | '\'' => quote = Some(ch),
            '(' => depth += 1,
            ')' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some((&value[1..offset], &value[offset + 1..]));
                }
            }
            _ => {}
        }
    }
    None
}

fn split_function_arguments(value: &str) -> Vec<&str> {
    let mut result = Vec::new();
    let mut start = 0;
    let mut depth = 0usize;
    let mut quote = None;
    let mut escaped = false;
    for (offset, ch) in value.char_indices() {
        if let Some(open) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == open {
                quote = None;
            }
            continue;
        }
        match ch {
            '"' | '\'' => quote = Some(ch),
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                result.push(value[start..offset].trim());
                start = offset + 1;
            }
            _ => {}
        }
    }
    result.push(value[start..].trim());
    result
}

fn valid_generated_counter_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(is_css_ident_char)
}

fn parse_generated_counter_style(value: &str) -> Option<crate::GeneratedCounterStyle> {
    Some(match value.to_ascii_lowercase().as_str() {
        "decimal" => crate::GeneratedCounterStyle::Decimal,
        "decimal-leading-zero" => crate::GeneratedCounterStyle::DecimalLeadingZero,
        "lower-alpha" | "lower-latin" => crate::GeneratedCounterStyle::LowerAlpha,
        "upper-alpha" | "upper-latin" => crate::GeneratedCounterStyle::UpperAlpha,
        "lower-roman" => crate::GeneratedCounterStyle::LowerRoman,
        "upper-roman" => crate::GeneratedCounterStyle::UpperRoman,
        _ => return None,
    })
}

pub(crate) fn format_counter_value(value: i32, style: crate::GeneratedCounterStyle) -> String {
    match style {
        crate::GeneratedCounterStyle::Decimal => value.to_string(),
        crate::GeneratedCounterStyle::DecimalLeadingZero if (-9..=9).contains(&value) => {
            if value < 0 {
                format!("-{:02}", value.unsigned_abs())
            } else {
                format!("{value:02}")
            }
        }
        crate::GeneratedCounterStyle::DecimalLeadingZero => value.to_string(),
        crate::GeneratedCounterStyle::LowerAlpha => alpha_counter(value, false),
        crate::GeneratedCounterStyle::UpperAlpha => alpha_counter(value, true),
        crate::GeneratedCounterStyle::LowerRoman => roman_counter(value, false),
        crate::GeneratedCounterStyle::UpperRoman => roman_counter(value, true),
    }
}

fn alpha_counter(value: i32, uppercase: bool) -> String {
    if value <= 0 {
        return value.to_string();
    }
    let mut value = value as u32;
    let mut result = Vec::new();
    while value > 0 {
        value -= 1;
        result.push((b'a' + (value % 26) as u8) as char);
        value /= 26;
    }
    result.reverse();
    let result: String = result.into_iter().collect();
    if uppercase {
        result.to_ascii_uppercase()
    } else {
        result
    }
}

fn roman_counter(value: i32, uppercase: bool) -> String {
    if !(1..=3999).contains(&value) {
        return value.to_string();
    }
    let mut value = value;
    let mut result = String::new();
    for &(amount, numeral) in &[
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ] {
        while value >= amount {
            value -= amount;
            result.push_str(numeral);
        }
    }
    if uppercase {
        result
    } else {
        result.to_ascii_lowercase()
    }
}

/// Decode CSS string escapes: `\` followed by 1-6 hex digits is a Unicode
/// code point (`\200B` -> U+200B ZERO WIDTH SPACE, ubiquitous in generated
/// `content` for accessible section-edit-link brackets and similar), with a
/// single trailing whitespace character consumed as the escape's own
/// terminator per the CSS spec rather than treated as literal content;
/// anything else after a backslash (`\"`, `\\`) is a literal escaped
/// character. Without this, a hex escape prints as its own literal digits.
fn unescape_css_string(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let mut hex = String::new();
        while hex.len() < 6 {
            match chars.peek() {
                Some(h) if h.is_ascii_hexdigit() => {
                    hex.push(*h);
                    chars.next();
                }
                _ => break,
            }
        }
        if !hex.is_empty() {
            if matches!(chars.peek(), Some(next) if next.is_whitespace()) {
                chars.next();
            }
            if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                out.push(ch);
                continue;
            }
        }
        if let Some(next) = chars.next() {
            out.push(next);
        }
    }
    out
}

/// Split a stylesheet into `(selector, declarations)` rules. Handles nested
/// braces, `/* comments */`, and the at-rules that carry ordinary rules inside
/// (`@media`, `@supports`, `@layer`); other at-rules (`@font-face`, `@keyframes`, ...) are
/// skipped since they do not contribute layout-relevant declarations here.
pub fn parse_stylesheet(css: &str) -> Vec<(String, String)> {
    parse_stylesheet_for_viewport(css, (1280.0, 720.0))
}

fn parse_stylesheet_for_viewport(css: &str, viewport: (f32, f32)) -> Vec<(String, String)> {
    let mut conditions = vec![ContainerConditionNode {
        parent: ContainerConditionId::NONE,
        alternatives: Vec::new(),
    }];
    parse_stylesheet_for_viewport_preserving_containers(
        css,
        viewport,
        CssMediaType::Screen,
        &mut conditions,
        ContainerConditionId::NONE,
    )
    .into_iter()
    // This legacy tuple API cannot express conditional context. Keep its
    // established behavior by omitting unresolved container rules.
    .filter(|rule| {
        rule.container_condition_id == ContainerConditionId::NONE
            && !rule
                .selector
                .starts_with(PROPERTY_REGISTRATION_SELECTOR_PREFIX)
    })
    .map(|rule| (rule.selector, rule.declarations))
    .collect()
}

fn parse_stylesheet_for_viewport_preserving_containers(
    css: &str,
    viewport: (f32, f32),
    media_type: CssMediaType,
    container_conditions: &mut Vec<ContainerConditionNode>,
    container_condition_id: ContainerConditionId,
) -> Vec<ParsedRule> {
    let mut layers = LayerRegistry::default();
    parse_stylesheet_for_viewport_preserving_containers_in_layer(
        css,
        viewport,
        media_type,
        container_conditions,
        container_condition_id,
        &mut layers,
        None,
    )
}

fn parse_stylesheet_for_viewport_preserving_containers_in_layer(
    css: &str,
    viewport: (f32, f32),
    media_type: CssMediaType,
    container_conditions: &mut Vec<ContainerConditionNode>,
    container_condition_id: ContainerConditionId,
    layers: &mut LayerRegistry,
    current_layer: Option<LayerOrder>,
) -> Vec<ParsedRule> {
    let mut rules = Vec::new();
    let mut current_selector = String::new();
    let mut current_decls = String::new();
    let mut block_depth = 0;
    let mut in_comment = false;
    let mut chars = css.chars().peekable();

    while let Some(c) = chars.next() {
        if in_comment {
            if c == '*' && chars.peek() == Some(&'/') {
                chars.next();
                in_comment = false;
            }
            continue;
        }
        if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            in_comment = true;
            continue;
        }

        if c == '{' {
            if block_depth != 0 {
                current_decls.push(c);
            }
            block_depth += 1;
        } else if c == '}' && block_depth == 0 {
            // Stray top-level close brace (unbalanced author CSS; remoteok.com
            // ships one mid-sheet). Browsers error-recover and keep parsing;
            // without this block_depth goes negative and the state machine
            // inverts, scrambling and losing every rule in the rest of the sheet.
            current_selector.clear();
        } else if c == '}' {
            block_depth -= 1;
            if block_depth == 0 {
                let sel = current_selector.trim();
                let decls = current_decls.trim();
                if let Some(at) = sel.strip_prefix('@') {
                    flush_at_rule(
                        at,
                        sel,
                        decls,
                        &mut rules,
                        viewport,
                        media_type,
                        container_conditions,
                        container_condition_id,
                        layers,
                        current_layer.as_ref(),
                    );
                } else {
                    // The body may contain nested rules (CSS Nesting, ubiquitous
                    // in Tailwind v4 / modern frameworks: `.a{ &:hover{} .b{} }`).
                    // Flatten them against this selector; denest also handles the
                    // no-nesting case (just emits the rule's own declarations).
                    denest(
                        sel,
                        decls,
                        &mut rules,
                        viewport,
                        media_type,
                        container_conditions,
                        container_condition_id,
                        layers,
                        current_layer.as_ref(),
                    );
                }
                current_selector.clear();
                current_decls.clear();
            } else {
                current_decls.push(c);
            }
        } else if c == ';' && block_depth == 0 {
            // Layer ordering statements establish slots even though they emit
            // no selector rules. All other statement at-rules are discarded so
            // their prelude cannot bleed into the next selector.
            if let Some(at) = current_selector.trim().strip_prefix('@') {
                if let Some(prelude) = at_rule_prelude(at, "layer") {
                    layers.register_statement(current_layer.as_ref(), prelude);
                }
            }
            current_selector.clear();
        } else if block_depth > 0 {
            current_decls.push(c);
        } else {
            current_selector.push(c);
        }
    }
    rules
}

/// Handle the at-rules whose bodies contain ordinary rules. For `@media`, apply
/// the inner rules only when the query holds for a desktop 1280px viewport.
fn flush_at_rule(
    at: &str,
    _sel: &str,
    inner: &str,
    rules: &mut Vec<ParsedRule>,
    viewport: (f32, f32),
    media_type: CssMediaType,
    container_conditions: &mut Vec<ContainerConditionNode>,
    container_condition_id: ContainerConditionId,
    layers: &mut LayerRegistry,
    current_layer: Option<&LayerOrder>,
) {
    if let Some(prelude) = at_rule_prelude(at, "media") {
        if media_query_applies_for_viewport_and_type(prelude, viewport, media_type) {
            rules.extend(
                parse_stylesheet_for_viewport_preserving_containers_in_layer(
                    inner,
                    viewport,
                    media_type,
                    container_conditions,
                    container_condition_id,
                    layers,
                    current_layer.cloned(),
                ),
            );
        }
    } else if let Some(prelude) = at_rule_prelude(at, "supports") {
        if supports_condition_applies(prelude) {
            rules.extend(
                parse_stylesheet_for_viewport_preserving_containers_in_layer(
                    inner,
                    viewport,
                    media_type,
                    container_conditions,
                    container_condition_id,
                    layers,
                    current_layer.cloned(),
                ),
            );
        }
    } else if let Some(prelude) = at_rule_prelude(at, "container") {
        if let Some(alternatives) = parse_container_query_list(prelude) {
            let Ok(raw_id) = u32::try_from(container_conditions.len()) else {
                return;
            };
            let id = ContainerConditionId(raw_id);
            container_conditions.push(ContainerConditionNode {
                parent: container_condition_id,
                alternatives,
            });
            rules.extend(
                parse_stylesheet_for_viewport_preserving_containers_in_layer(
                    inner,
                    viewport,
                    media_type,
                    container_conditions,
                    id,
                    layers,
                    current_layer.cloned(),
                ),
            );
        }
    } else if let Some((name, prefixed)) = at_rule_prelude(at, "keyframes")
        .map(|name| (name, false))
        .or_else(|| at_rule_prelude(at, "-webkit-keyframes").map(|name| (name, true)))
    {
        let name = name.trim();
        if !name.is_empty() {
            rules.push(ParsedRule {
                selector: format!(
                    "{}{name}",
                    if prefixed {
                        WEBKIT_KEYFRAMES_SELECTOR_PREFIX
                    } else {
                        KEYFRAMES_SELECTOR_PREFIX
                    }
                ),
                declarations: inner.to_string(),
                container_condition_id: ContainerConditionId::NONE,
                layer: current_layer.cloned(),
            });
        }
    } else if let Some(name) = at_rule_prelude(at, "property") {
        if name.starts_with("--") {
            rules.push(ParsedRule {
                selector: format!("{PROPERTY_REGISTRATION_SELECTOR_PREFIX}{name}"),
                declarations: inner.to_string(),
                // Registrations are global name-defining rules. CSS
                // Conditional 5 deliberately does not gate them on an
                // enclosing container query.
                container_condition_id: ContainerConditionId::NONE,
                layer: None,
            });
        }
    } else if let Some(prelude) = at_rule_prelude(at, "layer") {
        let layer = if prelude.trim().is_empty() {
            layers.register_anonymous(current_layer)
        } else if let Some(layer) = layers.register_named(current_layer, prelude) {
            layer
        } else {
            return;
        };
        rules.extend(
            parse_stylesheet_for_viewport_preserving_containers_in_layer(
                inner,
                viewport,
                media_type,
                container_conditions,
                container_condition_id,
                layers,
                Some(layer),
            ),
        );
    }
    // Other at-rules (@font-face, @import, ...) carry no
    // layout-relevant rules for us, so drop them.
}

fn parse_property_registration(descriptors: &str) -> Option<RegisteredCustomProperty> {
    let mut syntax = None;
    let mut inherits = None;
    let mut initial_value = None;
    for declaration in crate::style::split_declarations(descriptors) {
        let Some((name, value)) = declaration.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "syntax" => {
                let unquoted = value
                    .strip_prefix('"')
                    .and_then(|value| value.strip_suffix('"'))
                    .or_else(|| {
                        value
                            .strip_prefix('\'')
                            .and_then(|value| value.strip_suffix('\''))
                    })?;
                if !unquoted.is_empty() {
                    syntax = Some(unquoted.to_string());
                }
            }
            "inherits" => {
                inherits = match value.to_ascii_lowercase().as_str() {
                    "true" => Some(true),
                    "false" => Some(false),
                    _ => None,
                };
            }
            "initial-value" if !value.is_empty() => initial_value = Some(value.to_string()),
            _ => {}
        }
    }
    let syntax = syntax?;
    if !matches!(
        syntax.as_str(),
        "*" | "<percentage>" | "<length>" | "<number>" | "<color>"
    ) {
        return None;
    }
    if initial_value.is_none() && syntax != "*" {
        return None;
    }
    let registration = RegisteredCustomProperty {
        syntax,
        inherits: inherits?,
        initial_value,
    };
    if registration
        .initial_value
        .as_deref()
        .is_some_and(|value| !registered_value_matches(&registration, value))
    {
        return None;
    }
    Some(registration)
}

fn registered_value_matches(registration: &RegisteredCustomProperty, value: &str) -> bool {
    let value = value.trim();
    match registration.syntax.as_str() {
        "*" => !value.is_empty(),
        "<percentage>" => value
            .strip_suffix('%')
            .and_then(|number| number.trim().parse::<f32>().ok())
            .is_some_and(f32::is_finite),
        "<number>" => value.parse::<f32>().ok().is_some_and(f32::is_finite),
        "<color>" => crate::style::parse_color(value).is_some(),
        "<length>" => {
            if value
                .parse::<f32>()
                .ok()
                .is_some_and(|number| number == 0.0)
            {
                return true;
            }
            let lower = value.to_ascii_lowercase();
            [
                "rem", "em", "ex", "vmin", "vmax", "dvw", "svw", "lvw", "dvh", "svh", "lvh", "vw",
                "vh", "px", "pt",
            ]
            .iter()
            .any(|unit| {
                lower
                    .strip_suffix(unit)
                    .and_then(|number| number.trim().parse::<f32>().ok())
                    .is_some_and(f32::is_finite)
            })
        }
        _ => false,
    }
}

/// Return the prelude after an exact ASCII-insensitive at-rule name.
///
/// The hand parser stores the text after `@`, so a prefix test would both
/// reject `@CONTAINER` and misclassify unknown rules such as
/// `@containerfoo`. The boundary accepts punctuation because whitespace is
/// optional before a parenthesized prelude.
fn at_rule_prelude<'a>(at: &'a str, expected: &str) -> Option<&'a str> {
    let prefix = at.get(..expected.len())?;
    if !prefix.eq_ignore_ascii_case(expected) {
        return None;
    }
    let rest = &at[expected.len()..];
    if rest.chars().next().is_some_and(|character| {
        character.is_ascii_alphanumeric()
            || matches!(character, '_' | '-' | '\\')
            || !character.is_ascii()
    }) {
        return None;
    }
    let mut rest = rest.trim_start();
    while let Some(comment) = rest.strip_prefix("/*") {
        let end = comment.find("*/")?;
        rest = comment[end + 2..].trim_start();
    }
    Some(rest.trim_end())
}

fn parse_container_query_list(prelude: &str) -> Option<Vec<ContainerQuery>> {
    let queries = split_media_query_list(prelude)
        .into_iter()
        .map(parse_container_query)
        .collect::<Option<Vec<_>>>()?;
    (!queries.is_empty()).then_some(queries)
}

fn parse_container_query(input: &str) -> Option<ContainerQuery> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    let starts_with_condition =
        input.starts_with('(') || strip_ascii_keyword(input, "not").is_some();
    let (name, condition) = if starts_with_condition {
        (None, Some(input))
    } else if let Some(split) = input.find(char::is_whitespace) {
        let name = parse_container_query_name(&input[..split])?;
        let condition = input[split..].trim();
        (Some(name), (!condition.is_empty()).then_some(condition))
    } else {
        (Some(parse_container_query_name(input)?), None)
    };
    let condition = match condition {
        Some(condition) => Some(parse_container_query_expr(condition)?),
        None => None,
    };
    Some(ContainerQuery { name, condition })
}

fn parse_container_query_name(input: &str) -> Option<String> {
    let mut input = cssparser::ParserInput::new(input.trim());
    let mut parser = cssparser::Parser::new(&mut input);
    let ident = parser.expect_ident_cloned().ok()?;
    if !parser.is_exhausted() {
        return None;
    }
    let lower = ident.to_ascii_lowercase();
    if is_reserved_container_custom_ident(&lower) {
        return None;
    }
    Some(ident.to_string())
}

fn is_reserved_container_custom_ident(lower: &str) -> bool {
    matches!(
        lower,
        "none"
            | "not"
            | "and"
            | "or"
            | "default"
            | "initial"
            | "inherit"
            | "unset"
            | "revert"
            | "revert-layer"
    )
}

const MAX_CONTAINER_QUERY_DEPTH: usize = 64;

fn parse_container_query_expr(input: &str) -> Option<ContainerQueryExpr> {
    parse_container_query_expr_at_depth(input, 0)
}

fn parse_container_query_expr_at_depth(input: &str, depth: usize) -> Option<ContainerQueryExpr> {
    if depth >= MAX_CONTAINER_QUERY_DEPTH {
        return None;
    }
    let input = input.trim();
    let or_parts = split_supports_operator(input, "or");
    let and_parts = split_supports_operator(input, "and");
    // One grammar level is either a homogeneous AND chain or a homogeneous OR
    // chain. Authors must parenthesize any mixture.
    if or_parts.is_some() && and_parts.is_some() {
        return None;
    }
    if let Some(parts) = or_parts {
        return Some(ContainerQueryExpr::Or(
            parts
                .into_iter()
                .map(|part| parse_container_query_in_parens(part, depth + 1))
                .collect::<Option<_>>()?,
        ));
    }
    if let Some(parts) = and_parts {
        return Some(ContainerQueryExpr::And(
            parts
                .into_iter()
                .map(|part| parse_container_query_in_parens(part, depth + 1))
                .collect::<Option<_>>()?,
        ));
    }
    if let Some(rest) = strip_ascii_keyword(input, "not") {
        return Some(ContainerQueryExpr::Not(Box::new(
            parse_container_query_in_parens(rest, depth + 1)?,
        )));
    }
    parse_container_query_in_parens(input, depth + 1)
}

fn parse_container_query_in_parens(input: &str, depth: usize) -> Option<ContainerQueryExpr> {
    if depth >= MAX_CONTAINER_QUERY_DEPTH {
        return None;
    }
    let inner = enclosing_parenthesized(input)?;
    if let Some(feature) = parse_container_size_feature(inner) {
        return Some(feature);
    }
    parse_container_query_expr_at_depth(inner, depth + 1).or_else(|| {
        is_general_enclosed_container_query(inner).then_some(ContainerQueryExpr::Unknown)
    })
}

fn is_general_enclosed_container_query(input: &str) -> bool {
    let mut input = cssparser::ParserInput::new(input.trim());
    let mut parser = cssparser::Parser::new(&mut input);
    matches!(
        parser.next(),
        Ok(cssparser::Token::Ident(_)) | Ok(cssparser::Token::Function(_))
    )
}

fn strip_ascii_keyword<'a>(input: &'a str, keyword: &str) -> Option<&'a str> {
    if !input.get(..keyword.len())?.eq_ignore_ascii_case(keyword) {
        return None;
    }
    let rest = &input[keyword.len()..];
    rest.chars()
        .next()
        .filter(|character| character.is_whitespace())
        .map(|_| rest.trim_start())
}

fn parse_container_size_feature(input: &str) -> Option<ContainerQueryExpr> {
    if let Some(axis) = parse_container_query_axis(input) {
        return Some(ContainerQueryExpr::Feature(ContainerSizeFeature {
            axis,
            comparison: ContainerQueryComparison::GreaterThan,
            length: ContainerQueryLength::Px(0.0),
        }));
    }
    if let Some((name, value)) = input.split_once(':') {
        let (comparison, axis) = match name.trim().to_ascii_lowercase().as_str() {
            "min-width" => (ContainerQueryComparison::Min, ContainerQueryAxis::Width),
            "max-width" => (ContainerQueryComparison::Max, ContainerQueryAxis::Width),
            "width" => (ContainerQueryComparison::Equal, ContainerQueryAxis::Width),
            "min-height" => (ContainerQueryComparison::Min, ContainerQueryAxis::Height),
            "max-height" => (ContainerQueryComparison::Max, ContainerQueryAxis::Height),
            "height" => (ContainerQueryComparison::Equal, ContainerQueryAxis::Height),
            "min-inline-size" => (
                ContainerQueryComparison::Min,
                ContainerQueryAxis::InlineSize,
            ),
            "max-inline-size" => (
                ContainerQueryComparison::Max,
                ContainerQueryAxis::InlineSize,
            ),
            "inline-size" => (
                ContainerQueryComparison::Equal,
                ContainerQueryAxis::InlineSize,
            ),
            "min-block-size" => (ContainerQueryComparison::Min, ContainerQueryAxis::BlockSize),
            "max-block-size" => (ContainerQueryComparison::Max, ContainerQueryAxis::BlockSize),
            "block-size" => (
                ContainerQueryComparison::Equal,
                ContainerQueryAxis::BlockSize,
            ),
            _ => return None,
        };
        return Some(ContainerQueryExpr::Feature(ContainerSizeFeature {
            axis,
            comparison,
            length: parse_container_query_length(value)?,
        }));
    }

    let (operands, operators) = split_container_range(input)?;
    let make_feature =
        |axis: ContainerQueryAxis, operator: &str, value: &str, axis_on_left: bool| {
            let comparison = match (operator, axis_on_left) {
                (">=", true) | ("<=", false) => ContainerQueryComparison::Min,
                ("<=", true) | (">=", false) => ContainerQueryComparison::Max,
                (">", true) | ("<", false) => ContainerQueryComparison::GreaterThan,
                ("<", true) | (">", false) => ContainerQueryComparison::LessThan,
                ("=", _) => ContainerQueryComparison::Equal,
                _ => return None,
            };
            Some(ContainerQueryExpr::Feature(ContainerSizeFeature {
                axis,
                comparison,
                length: parse_container_query_length(value)?,
            }))
        };
    match (operands.as_slice(), operators.as_slice()) {
        ([left, right], [operator]) => {
            if let Some(axis) = parse_container_query_axis(left) {
                make_feature(axis, operator, right, true)
            } else {
                let axis = parse_container_query_axis(right)?;
                make_feature(axis, operator, left, false)
            }
        }
        ([lower, middle, upper], [lower_operator, upper_operator]) => {
            // Chained ranges must point consistently through the feature:
            // `10px < width <= 20px` or the fully reversed equivalent.
            // Equality is valid only in a single comparison, and a mixed
            // direction such as `10px < width > 20px` is not a range.
            let forward =
                matches!(*lower_operator, "<" | "<=") && matches!(*upper_operator, "<" | "<=");
            let reverse =
                matches!(*lower_operator, ">" | ">=") && matches!(*upper_operator, ">" | ">=");
            if !forward && !reverse {
                return None;
            }
            let axis = parse_container_query_axis(middle)?;
            Some(ContainerQueryExpr::And(vec![
                make_feature(axis, lower_operator, lower, false)?,
                make_feature(axis, upper_operator, upper, true)?,
            ]))
        }
        _ => None,
    }
}

fn parse_container_query_axis(input: &str) -> Option<ContainerQueryAxis> {
    match input.trim().to_ascii_lowercase().as_str() {
        "width" => Some(ContainerQueryAxis::Width),
        "height" => Some(ContainerQueryAxis::Height),
        "inline-size" => Some(ContainerQueryAxis::InlineSize),
        "block-size" => Some(ContainerQueryAxis::BlockSize),
        _ => None,
    }
}

fn split_container_range(input: &str) -> Option<(Vec<&str>, Vec<&str>)> {
    let mut operands = Vec::new();
    let mut operators = Vec::new();
    let mut start = 0usize;
    let bytes = input.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        if matches!(bytes[index], b'<' | b'>' | b'=') {
            let end = if bytes.get(index + 1) == Some(&b'=') {
                index + 2
            } else {
                index + 1
            };
            let operand = input[start..index].trim();
            if operand.is_empty() {
                return None;
            }
            operands.push(operand);
            operators.push(&input[index..end]);
            start = end;
            index = end;
            continue;
        }
        index += 1;
    }
    if operators.is_empty() || operators.len() > 2 {
        return None;
    }
    let operand = input[start..].trim();
    if operand.is_empty() {
        return None;
    }
    operands.push(operand);
    Some((operands, operators))
}

fn parse_container_query_length(input: &str) -> Option<ContainerQueryLength> {
    let input = input.trim().to_ascii_lowercase();
    let number = |value: &str| value.parse::<f32>().ok().filter(|value| value.is_finite());
    if let Some(value) = input.strip_suffix("rem").and_then(number) {
        return Some(ContainerQueryLength::Rem(value));
    }
    if let Some(value) = input.strip_suffix("em").and_then(number) {
        return Some(ContainerQueryLength::Em(value));
    }
    if let Some(value) = input.strip_suffix("px").and_then(number) {
        return Some(ContainerQueryLength::Px(value));
    }
    number(&input)
        .filter(|value| *value == 0.0)
        .map(ContainerQueryLength::Px)
}

/// Evaluate the boolean subset of CSS Conditional Rules used by modern
/// framework stylesheets. This follows the same shape as Gecko's
/// SupportsCondition tree: declaration/selector leaves, arbitrary grouping,
/// unary `not`, and top-level `and`/`or` chains. Unknown or malformed future
/// syntax is false instead of optimistically enabling a fallback branch.
pub(crate) fn supports_condition_applies(condition: &str) -> bool {
    let condition = condition.trim();
    let condition = condition
        .strip_prefix("@supports")
        .or_else(|| condition.strip_prefix("supports"))
        .unwrap_or(condition)
        .trim();
    eval_supports_condition(condition).unwrap_or(false)
}

fn eval_supports_condition(condition: &str) -> Option<bool> {
    let condition = condition.trim();
    if condition.is_empty() || !balanced_supports_syntax(condition) {
        return None;
    }
    if let Some(inner) = enclosing_parenthesized(condition) {
        return eval_supports_condition(inner);
    }
    if condition
        .get(..3)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("not"))
        && condition
            .as_bytes()
            .get(3)
            .is_some_and(u8::is_ascii_whitespace)
    {
        return eval_supports_condition(condition[3..].trim()).map(|result| !result);
    }
    let or_parts = split_supports_operator(condition, "or");
    let and_parts = split_supports_operator(condition, "and");
    // CSS Conditional Rules does not permit `and` and `or` at the same
    // nesting level. Treat the entire condition as invalid rather than
    // inventing JavaScript-like precedence.
    if or_parts.is_some() && and_parts.is_some() {
        return None;
    }
    if let Some(parts) = or_parts {
        let results = parts
            .into_iter()
            .map(eval_supports_condition)
            .collect::<Option<Vec<_>>>()?;
        return Some(results.into_iter().any(|result| result));
    }
    if let Some(parts) = and_parts {
        let results = parts
            .into_iter()
            .map(eval_supports_condition)
            .collect::<Option<Vec<_>>>()?;
        return Some(results.into_iter().all(|result| result));
    }
    let lower = condition.to_ascii_lowercase();
    if lower.starts_with("selector(") && condition.ends_with(')') {
        let inner = &condition["selector(".len()..condition.len() - 1];
        return Some(obscura_dom::selector::parse_selector(inner.trim()).is_ok());
    }
    let Some((name, value)) = condition.split_once(':') else {
        // A syntactically balanced unknown functional condition is a valid
        // general-enclosed leaf whose result is false. Everything else is a
        // malformed condition, important for `not`: invalid syntax must not
        // become true merely because it was negated.
        return condition
            .find('(')
            .filter(|index| *index > 0 && condition.ends_with(')'))
            .map(|_| false);
    };
    Some(crate::style::supports_declaration(name, value))
}

fn balanced_supports_syntax(condition: &str) -> bool {
    let mut stack = Vec::new();
    let mut quote = None;
    let mut escaped = false;
    for character in condition.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if let Some(active) = quote {
            if character == active {
                quote = None;
            }
            continue;
        }
        match character {
            '\'' | '"' => quote = Some(character),
            '(' => stack.push(')'),
            '[' => stack.push(']'),
            ')' | ']' if stack.pop() != Some(character) => return false,
            _ => {}
        }
    }
    !escaped && quote.is_none() && stack.is_empty()
}

/// Return the contents when one outer parenthesis pair encloses the complete
/// expression. A declaration leaf such as `(display:grid)` intentionally
/// becomes `display:grid` and is handled after boolean operators.
fn enclosing_parenthesized(condition: &str) -> Option<&str> {
    if !condition.starts_with('(') {
        return None;
    }
    let mut depth = 0i32;
    let mut quote = None;
    for (index, character) in condition.char_indices() {
        if let Some(active) = quote {
            if character == active {
                quote = None;
            }
            continue;
        }
        match character {
            '\'' | '"' => quote = Some(character),
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth < 0 {
                    return None;
                }
                if depth == 0 {
                    return (index + character.len_utf8() == condition.len())
                        .then_some(&condition[1..index]);
                }
            }
            _ => {}
        }
    }
    None
}

fn split_supports_operator<'a>(condition: &'a str, operator: &str) -> Option<Vec<&'a str>> {
    let bytes = condition.as_bytes();
    let operator = operator.as_bytes();
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut index = 0usize;
    let mut depth = 0i32;
    let mut quote = None;
    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(active) = quote {
            if byte == active {
                quote = None;
            } else if byte == b'\\' {
                index += 1;
            }
            index += 1;
            continue;
        }
        match byte {
            b'\'' | b'"' => quote = Some(byte),
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth < 0 {
                    return None;
                }
            }
            _ if depth == 0
                && index + operator.len() <= bytes.len()
                && bytes[index..index + operator.len()].eq_ignore_ascii_case(operator)
                && index > 0
                && bytes[index - 1].is_ascii_whitespace()
                && bytes
                    .get(index + operator.len())
                    .is_some_and(u8::is_ascii_whitespace) =>
            {
                let part = condition[start..index].trim();
                if part.is_empty() {
                    return None;
                }
                parts.push(part);
                index += operator.len();
                start = index;
                continue;
            }
            _ => {}
        }
        index += 1;
    }
    if depth != 0 || quote.is_some() || parts.is_empty() {
        return None;
    }
    let tail = condition[start..].trim();
    if tail.is_empty() {
        return None;
    }
    parts.push(tail);
    Some(parts)
}

/// Coarse `@media` evaluation against the live layout viewport.
///
/// Real stylesheets format media features inconsistently
/// (`max-width:750px`, `max-width: 750px`, even `max-width : 750px`), so this
/// strips whitespace before scanning: CSS gives no semantic meaning to spaces
/// inside `(feature: value)`, so it's safe to discard them wholesale rather
/// than special-case every formatting variant a site might use.
pub(crate) fn media_query_applies_for_viewport(query: &str, viewport: (f32, f32)) -> bool {
    media_query_applies_for_viewport_and_type(query, viewport, CssMediaType::Screen)
}

pub(crate) fn media_query_applies_for_viewport_and_type(
    query: &str,
    viewport: (f32, f32),
    media_type: CssMediaType,
) -> bool {
    // A media-query list is an OR, not an AND. Evaluate each top-level comma
    // arm independently (commas inside functions such as rgb() / calc() are
    // not list separators). This also keeps an inapplicable `print` arm from
    // suppressing a later screen/feature arm.
    split_media_query_list(query)
        .into_iter()
        .any(|query| single_media_query_applies_for_viewport(query, viewport, media_type))
}

fn single_media_query_applies_for_viewport(
    query: &str,
    viewport: (f32, f32),
    media_type: CssMediaType,
) -> bool {
    let viewport_w = viewport.0;
    let viewport_h = viewport.1;
    let query = query.trim().strip_prefix("@media").unwrap_or(query).trim();
    let compact: String = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(|c| c.to_lowercase())
        .collect();

    // A leading `not` negates the complete media query. This covers both
    // Tailwind's `not all and (...)` breakpoints and ordinary `not print` /
    // `not screen` selection without treating a word inside a feature as a
    // media type.
    if let Some(inner) = compact.strip_prefix("not") {
        return !single_media_query_applies_for_viewport(inner, viewport, media_type);
    }
    let compact = compact.strip_prefix("only").unwrap_or(&compact);

    let medium = compact.split_once("and").map_or(compact, |(medium, _)| medium);
    let medium_matches = match medium {
        "all" => true,
        "screen" => media_type == CssMediaType::Screen,
        "print" => media_type == CssMediaType::Print,
        medium if medium.starts_with('(') => true,
        // Unknown named media such as `speech` do not match either visual
        // rendering mode.
        _ => false,
    };
    if !medium_matches {
        return false;
    }

    // Color-scheme: we render the light (default) context. A site's
    // `@media (prefers-color-scheme: dark)` block must NOT apply on top of its
    // light defaults (that is what was leaking dark backgrounds, e.g. near
    // black inline <code>); a `:light` block should apply.
    if compact.contains("prefers-color-scheme:dark") {
        return false;
    }
    // Reduced-motion / high-contrast / inverted: default (no preference).
    if compact.contains("prefers-reduced-motion:reduce")
        || compact.contains("prefers-contrast:more")
        || compact.contains("prefers-contrast:less")
        || compact.contains("inverted-colors:inverted")
        || compact.contains("forced-colors:active")
    {
        return false;
    }

    // Width constraints, both `min-width:`/`max-width:` and the modern range
    // forms `width>=Npx` / `(Npx<=width)`.
    if let Some(px) = extract_length(&compact, "max-width:", viewport, LengthAxis::Width) {
        if viewport_w > px {
            return false;
        }
    }
    if let Some(px) = extract_length(&compact, "min-width:", viewport, LengthAxis::Width) {
        if viewport_w < px {
            return false;
        }
    }
    if let Some(px) = extract_length(&compact, "width<=", viewport, LengthAxis::Width) {
        if viewport_w > px {
            return false;
        }
    }
    if let Some(px) = extract_length(&compact, "width>=", viewport, LengthAxis::Width) {
        if viewport_w < px {
            return false;
        }
    }
    if let Some(px) = extract_length(&compact, "width>", viewport, LengthAxis::Width) {
        if viewport_w <= px {
            return false;
        }
    }
    if let Some(px) = extract_length(&compact, "width<", viewport, LengthAxis::Width) {
        if viewport_w >= px {
            return false;
        }
    }
    if let Some(px) = extract_length_before(&compact, "<=width", viewport, LengthAxis::Width) {
        if viewport_w < px {
            return false;
        }
    }
    if let Some(px) = extract_length_before(&compact, "<width", viewport, LengthAxis::Width) {
        if viewport_w <= px {
            return false;
        }
    }
    if let Some(px) = extract_length_before(&compact, ">=width", viewport, LengthAxis::Width) {
        if viewport_w > px {
            return false;
        }
    }
    if let Some(px) = extract_length_before(&compact, ">width", viewport, LengthAxis::Width) {
        if viewport_w >= px {
            return false;
        }
    }

    if let Some(px) = extract_length(&compact, "max-height:", viewport, LengthAxis::Height) {
        if viewport_h > px {
            return false;
        }
    }
    if let Some(px) = extract_length(&compact, "min-height:", viewport, LengthAxis::Height) {
        if viewport_h < px {
            return false;
        }
    }
    if let Some(px) = extract_length(&compact, "height<=", viewport, LengthAxis::Height) {
        if viewport_h > px {
            return false;
        }
    }
    if let Some(px) = extract_length(&compact, "height>=", viewport, LengthAxis::Height) {
        if viewport_h < px {
            return false;
        }
    }
    if let Some(px) = extract_length(&compact, "height>", viewport, LengthAxis::Height) {
        if viewport_h <= px {
            return false;
        }
    }
    if let Some(px) = extract_length(&compact, "height<", viewport, LengthAxis::Height) {
        if viewport_h >= px {
            return false;
        }
    }
    if let Some(px) = extract_length_before(&compact, "<=height", viewport, LengthAxis::Height) {
        if viewport_h < px {
            return false;
        }
    }
    if let Some(px) = extract_length_before(&compact, "<height", viewport, LengthAxis::Height) {
        if viewport_h <= px {
            return false;
        }
    }
    if let Some(px) = extract_length_before(&compact, ">=height", viewport, LengthAxis::Height) {
        if viewport_h > px {
            return false;
        }
    }
    if let Some(px) = extract_length_before(&compact, ">height", viewport, LengthAxis::Height) {
        if viewport_h >= px {
            return false;
        }
    }
    if compact.contains("orientation:portrait") && viewport_w > viewport_h {
        return false;
    }
    if compact.contains("orientation:landscape") && viewport_h > viewport_w {
        return false;
    }
    true
}

/// Combine a nested selector `child` with its enclosing `parent` (CSS Nesting).
/// `&` in the child is replaced by the parent; a child with no `&` becomes a
/// descendant (`parent child`). Both may be comma lists, so the result is the
/// cartesian product. `parent` is None only at the stylesheet top level, where
/// the child is returned unchanged.
fn combine_selectors(parent: &str, child: &str) -> String {
    let pparts = split_selector_list(parent);
    let cparts = split_selector_list(child);
    let mut out: Vec<String> = Vec::new();
    for c in &cparts {
        let c = c.trim();
        if c.is_empty() {
            continue;
        }
        for p in &pparts {
            let p = p.trim();
            if c.contains('&') {
                out.push(c.replace('&', p));
            } else {
                out.push(format!("{} {}", p, c));
            }
        }
    }
    out.join(", ")
}

/// Flatten a rule body that may contain nested rules (CSS Nesting) into flat
/// `(selector, declarations)` pairs. The parser hands us the whole body of a
/// rule; here we separate its own declarations from nested `sel { ... }` blocks
/// (and nested `@media`/`@supports`/`@layer` at-rules, which keep the parent's
/// selector), emit `(sel, own-declarations)`, and recurse into each nested rule
/// with the combined selector. Without this, Tailwind v4 / modern-framework CSS
/// (which nests almost everything) loses the nested utility rules entirely.
fn denest(
    sel: &str,
    body: &str,
    rules: &mut Vec<ParsedRule>,
    viewport: (f32, f32),
    media_type: CssMediaType,
    container_conditions: &mut Vec<ContainerConditionNode>,
    container_condition_id: ContainerConditionId,
    layers: &mut LayerRegistry,
    current_layer: Option<&LayerOrder>,
) {
    let chars: Vec<char> = body.chars().collect();
    let n = chars.len();
    let mut i = 0;
    let mut seg = 0; // start of the current declaration / nested prelude
    let mut own = String::new();
    let mut quote: Option<char> = None;
    let mut comment = false;
    let mut paren = 0i32;
    while i < n {
        let c = chars[i];
        if comment {
            if c == '*' && chars.get(i + 1) == Some(&'/') {
                comment = false;
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'*') {
            comment = true;
            i += 2;
            continue;
        }
        match c {
            '\'' | '"' => quote = Some(c),
            '(' => paren += 1,
            ')' => paren = (paren - 1).max(0),
            '{' if paren == 0 => {
                let prelude: String = chars[seg..i].iter().collect();
                // Find the matching close brace (quote/comment aware).
                let mut depth = 1;
                let mut j = i + 1;
                let (mut q2, mut cm2) = (None::<char>, false);
                while j < n && depth > 0 {
                    let cj = chars[j];
                    if cm2 {
                        if cj == '*' && chars.get(j + 1) == Some(&'/') {
                            cm2 = false;
                            j += 2;
                            continue;
                        }
                    } else if let Some(qq) = q2 {
                        if cj == qq {
                            q2 = None;
                        }
                    } else if cj == '/' && chars.get(j + 1) == Some(&'*') {
                        cm2 = true;
                        j += 2;
                        continue;
                    } else if cj == '\'' || cj == '"' {
                        q2 = Some(cj);
                    } else if cj == '{' {
                        depth += 1;
                    } else if cj == '}' {
                        depth -= 1;
                    }
                    j += 1;
                }
                let inner: String = chars[i + 1..j.saturating_sub(1).max(i + 1)]
                    .iter()
                    .collect();
                let pre = prelude.trim();
                if let Some(at) = pre.strip_prefix('@') {
                    // A nested at-rule keeps the enclosing selector for its body.
                    if let Some(prelude) = at_rule_prelude(at, "media") {
                        if media_query_applies_for_viewport_and_type(
                            prelude,
                            viewport,
                            media_type,
                        ) {
                            denest(
                                sel,
                                &inner,
                                rules,
                                viewport,
                                media_type,
                                container_conditions,
                                container_condition_id,
                                layers,
                                current_layer,
                            );
                        }
                    } else if let Some(prelude) = at_rule_prelude(at, "supports") {
                        if supports_condition_applies(prelude) {
                            denest(
                                sel,
                                &inner,
                                rules,
                                viewport,
                                media_type,
                                container_conditions,
                                container_condition_id,
                                layers,
                                current_layer,
                            );
                        }
                    } else if let Some(prelude) = at_rule_prelude(at, "container") {
                        if let Some(alternatives) = parse_container_query_list(prelude) {
                            if let Ok(raw_id) = u32::try_from(container_conditions.len()) {
                                let id = ContainerConditionId(raw_id);
                                container_conditions.push(ContainerConditionNode {
                                    parent: container_condition_id,
                                    alternatives,
                                });
                                denest(
                                    sel,
                                    &inner,
                                    rules,
                                    viewport,
                                    media_type,
                                    container_conditions,
                                    id,
                                    layers,
                                    current_layer,
                                );
                            }
                        }
                    } else if let Some(prelude) = at_rule_prelude(at, "layer") {
                        let layer = if prelude.trim().is_empty() {
                            Some(layers.register_anonymous(current_layer))
                        } else {
                            layers.register_named(current_layer, prelude)
                        };
                        if let Some(layer) = layer {
                            denest(
                                sel,
                                &inner,
                                rules,
                                viewport,
                                media_type,
                                container_conditions,
                                container_condition_id,
                                layers,
                                Some(&layer),
                            );
                        }
                    }
                } else if !pre.is_empty() {
                    let full = combine_selectors(sel, pre);
                    denest(
                        &full,
                        &inner,
                        rules,
                        viewport,
                        media_type,
                        container_conditions,
                        container_condition_id,
                        layers,
                        current_layer,
                    );
                }
                i = j;
                seg = i;
                continue;
            }
            ';' if paren == 0 => {
                let d: String = chars[seg..i].iter().collect();
                let d = d.trim();
                if let Some(at) = d.strip_prefix('@') {
                    if let Some(prelude) = at_rule_prelude(at, "layer") {
                        layers.register_statement(current_layer, prelude);
                    }
                } else if !d.is_empty() {
                    own.push_str(d);
                    own.push(';');
                }
                i += 1;
                seg = i;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    let tail: String = chars[seg..].iter().collect();
    let tail = tail.trim();
    if !tail.is_empty() && !tail.contains('{') {
        own.push_str(tail);
        own.push(';');
    }
    if !own.trim().is_empty() {
        for s in split_selector_list(sel) {
            let s = s.trim();
            if !s.is_empty() {
                rules.push(ParsedRule {
                    selector: s.to_string(),
                    declarations: own.clone(),
                    container_condition_id,
                    layer: current_layer.cloned(),
                });
            }
        }
    }
}

/// Split a CSS selector list at top-level commas only, leaving commas inside
/// `()` (e.g. `:is(a, b)`, `:not(.x, .y)`) and `[]` (`[attr="a,b"]`) and quoted
/// strings intact. A naive `split(',')` shatters those grouped selectors into
/// fragments that fail to compile, dropping the whole rule.
fn split_selector_list(sel: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth_paren = 0i32;
    let mut depth_brack = 0i32;
    let mut quote: Option<char> = None;
    let mut cur = String::new();
    let mut chars = sel.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                cur.push(c);
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            _ if quote == Some(c) => {
                quote = None;
                cur.push(c);
            }
            '"' | '\'' if quote.is_none() => {
                quote = Some(c);
                cur.push(c);
            }
            '(' if quote.is_none() => {
                depth_paren += 1;
                cur.push(c);
            }
            ')' if quote.is_none() => {
                depth_paren -= 1;
                cur.push(c);
            }
            '[' if quote.is_none() => {
                depth_brack += 1;
                cur.push(c);
            }
            ']' if quote.is_none() => {
                depth_brack -= 1;
                cur.push(c);
            }
            ',' if quote.is_none() && depth_paren == 0 && depth_brack == 0 => {
                out.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

#[derive(Clone, Copy)]
enum LengthAxis {
    Width,
    Height,
}

/// Read a CSS length immediately following `prop`. Media-query `em` and `rem`
/// units resolve against the initial font size (16 CSS px), not an element's
/// computed font. Modern utility frameworks deliberately use those units for
/// breakpoints, so treating only `px` as typed made every `min-width:64rem`
/// desktop rule unconditional.
fn extract_length(s: &str, prop: &str, viewport: (f32, f32), axis: LengthAxis) -> Option<f32> {
    let start = s.find(prop)? + prop.len();
    let rest = &s[start..];
    if let Some(inner) = rest.strip_prefix("calc(") {
        let end = matching_paren_end(inner)?;
        return eval_length_sum(&inner[..end], viewport, axis);
    }
    parse_length_prefix(rest, viewport, axis)
}

/// Find the closing parenthesis paired with an opening parenthesis immediately
/// before `input`. Nested CSS math groups must not terminate the outer calc.
fn matching_paren_end(input: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (index, character) in input.char_indices() {
        match character {
            '(' => depth += 1,
            ')' if depth == 0 => return Some(index),
            ')' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// Read the length immediately before a range marker (`64rem<=width`).
fn extract_length_before(
    s: &str,
    marker: &str,
    viewport: (f32, f32),
    axis: LengthAxis,
) -> Option<f32> {
    let end = s.find(marker)?;
    let prefix = &s[..end];
    let mut depth = 0usize;
    let mut start = 0usize;
    for (index, character) in prefix.char_indices().rev() {
        match character {
            ')' => depth += 1,
            '(' if depth > 0 => depth -= 1,
            '(' | ':' | ',' if depth == 0 => {
                start = index + character.len_utf8();
                break;
            }
            _ => {}
        }
    }
    let value = prefix[start..].trim();
    if let Some(inner) = value.strip_prefix("calc(") {
        let end = matching_paren_end(inner)?;
        return eval_length_sum(&inner[..end], viewport, axis);
    }
    parse_length_prefix(value, viewport, axis)
}

fn parse_length_prefix(input: &str, viewport: (f32, f32), axis: LengthAxis) -> Option<f32> {
    let numeric_len = input
        .char_indices()
        .take_while(|(_, c)| c.is_ascii_digit() || matches!(c, '.' | '+' | '-'))
        .last()
        .map_or(0, |(idx, c)| idx + c.len_utf8());
    if numeric_len == 0 {
        return None;
    }
    let value = input[..numeric_len].parse::<f32>().ok()?;
    let unit: String = input[numeric_len..]
        .chars()
        .take_while(|c| c.is_ascii_alphabetic() || *c == '%')
        .collect();
    let px = match unit.as_str() {
        "" | "px" => value,
        "em" | "rem" => value * 16.0,
        "vw" => value * viewport.0 / 100.0,
        "vh" => value * viewport.1 / 100.0,
        "vmin" => value * viewport.0.min(viewport.1) / 100.0,
        "vmax" => value * viewport.0.max(viewport.1) / 100.0,
        "in" => value * 96.0,
        "cm" => value * 96.0 / 2.54,
        "mm" => value * 96.0 / 25.4,
        "q" => value * 96.0 / 101.6,
        "pt" => value * 96.0 / 72.0,
        "pc" => value * 16.0,
        "%" => match axis {
            LengthAxis::Width => value * viewport.0 / 100.0,
            LengthAxis::Height => value * viewport.1 / 100.0,
        },
        _ => return None,
    };
    px.is_finite().then_some(px)
}

/// Resolve media-query `calc()` lengths with the same typed CSS math used by
/// layout. Real responsive breakpoints combine grouping and scalar arithmetic
/// (for example `calc(1rem * 2 + (15rem + 2rem) * 2 + 31rem)`), so a flat
/// plus/minus scanner silently turns those queries into unconditional rules.
fn eval_length_sum(expr: &str, viewport: (f32, f32), axis: LengthAxis) -> Option<f32> {
    let percent_base = match axis {
        LengthAxis::Width => viewport.0,
        LengthAxis::Height => viewport.1,
    };
    crate::style::resolve_contextual_length(
        &format!("calc({expr})"),
        16.0,
        16.0,
        viewport.0 / 100.0,
        viewport.1 / 100.0,
        percent_base,
    )
}

fn split_media_query_list(query: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut start = 0usize;
    for (idx, c) in query.char_indices() {
        match c {
            _ if quote == Some(c) => quote = None,
            '\'' | '"' if quote.is_none() => quote = Some(c),
            '(' if quote.is_none() => depth += 1,
            ')' if quote.is_none() => depth = (depth - 1).max(0),
            ',' if quote.is_none() && depth == 0 => {
                parts.push(query[start..idx].trim());
                start = idx + 1;
            }
            _ => {}
        }
    }
    parts.push(query[start..].trim());
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_invalidation_map(css: &str) -> InvalidationMap {
        let tree = obscura_dom::parse_html("<html><body></body></html>");
        Stylesheet::parse(&tree, &[css.to_string()])
            .invalidation_map()
            .clone()
    }

    fn dependencies_reach(
        dependencies: &[InvalidationDependency],
        reaches: InvalidationReaches,
    ) -> bool {
        dependencies
            .iter()
            .any(|dependency| dependency.reaches.contains(reaches))
    }

    #[test]
    fn invalidation_map_classifies_compound_reach_without_false_negatives() {
        let map = test_invalidation_map(
            r#"
                #self { color:red }
                #ancestor .subject { color:red }
                #parent > .child { color:red }
                #previous + .adjacent { color:red }
                #earlier ~ .later { color:red }
                .repeat .repeat { color:red }
            "#,
        );

        assert!(dependencies_reach(
            map.id_dependencies("self"),
            InvalidationReaches::SELF,
        ));
        assert!(dependencies_reach(
            map.id_dependencies("ancestor"),
            InvalidationReaches::DESCENDANTS,
        ));
        assert!(dependencies_reach(
            map.id_dependencies("parent"),
            InvalidationReaches::DESCENDANTS,
        ));
        assert!(dependencies_reach(
            map.id_dependencies("previous"),
            InvalidationReaches::SIBLINGS,
        ));
        assert!(dependencies_reach(
            map.id_dependencies("earlier"),
            InvalidationReaches::SIBLINGS,
        ));
        let repeated = map.class_dependencies("repeat");
        assert_eq!(repeated.len(), 1, "same-rule dependencies must merge");
        assert!(repeated[0].reaches.contains(InvalidationReaches::SELF));
        assert!(repeated[0]
            .reaches
            .contains(InvalidationReaches::DESCENDANTS));
        assert!(!map.requires_conservative_invalidation());
    }

    #[test]
    fn invalidation_map_distinguishes_tree_sibling_paths_from_has_paths() {
        let map = test_invalidation_map(
            r#"
                .left + .right { color:red }
                :is(.early ~ .late, .plain) { color:blue }
                .host:has(.inside + .peer) { color:green }
                [data-token="+"] { color:black }
            "#,
        );
        assert!(map.has_adjacent_sibling_selectors());
        assert!(map.has_general_sibling_selectors());

        let relational_only =
            test_invalidation_map(".host:has(.inside + .peer){color:red}");
        assert!(!relational_only.has_adjacent_sibling_selectors());
        assert!(!relational_only.has_general_sibling_selectors());
    }

    #[test]
    fn invalidation_map_indexes_attributes_states_functions_and_escaped_keys() {
        let map = test_invalidation_map(
            r#"
                [data-theme] .panel:hover { color:red }
                :is(.alpha, #beta) > button:focus-visible { color:red }
                button:not([disabled]) { color:red }
                .sm\:hover\:px-2[data-KIND] { color:red }
                .group:focus-within .icon { color:red }
            "#,
        );

        assert!(dependencies_reach(
            map.attribute_dependencies("DATA-THEME"),
            InvalidationReaches::DESCENDANTS,
        ));
        assert!(dependencies_reach(
            map.class_dependencies("panel"),
            InvalidationReaches::SELF,
        ));
        assert!(dependencies_reach(
            map.state_dependencies("hover"),
            InvalidationReaches::SELF,
        ));
        assert!(dependencies_reach(
            map.class_dependencies("alpha"),
            InvalidationReaches::DESCENDANTS,
        ));
        assert!(dependencies_reach(
            map.id_dependencies("beta"),
            InvalidationReaches::DESCENDANTS,
        ));
        assert!(dependencies_reach(
            map.state_dependencies("focus-visible"),
            InvalidationReaches::SELF,
        ));
        assert!(dependencies_reach(
            map.local_name_dependencies("BUTTON"),
            InvalidationReaches::SELF,
        ));
        assert!(dependencies_reach(
            map.attribute_dependencies("disabled"),
            InvalidationReaches::SELF,
        ));
        assert!(dependencies_reach(
            map.class_dependencies("sm:hover:px-2"),
            InvalidationReaches::SELF,
        ));
        assert!(dependencies_reach(
            map.attribute_dependencies("data-kind"),
            InvalidationReaches::SELF,
        ));
        assert!(dependencies_reach(
            map.state_dependencies("focus-within"),
            InvalidationReaches::DESCENDANTS,
        ));
        assert!(!map.requires_conservative_invalidation());
    }

    #[test]
    fn invalidation_map_indexes_declaration_attr_dependencies() {
        let map = test_invalidation_map(
            r#"
                .label::before { content: ATTR( DATA-LABEL ) }
                .typed { --label: attr(Aria-Label string, "fallback") }
                .noise::after {
                    content: "attr(data-in-string)";
                    background: url("attr(data-in-url)");
                    color: red /* attr(data-in-comment) */;
                }
            "#,
        );

        assert!(dependencies_reach(
            map.attribute_dependencies("data-label"),
            InvalidationReaches::SELF,
        ));
        assert!(dependencies_reach(
            map.attribute_dependencies("ARIA-LABEL"),
            InvalidationReaches::SELF,
        ));
        assert!(map.attribute_dependencies("data-in-string").is_empty());
        assert!(map.attribute_dependencies("data-in-url").is_empty());
        assert!(map.attribute_dependencies("data-in-comment").is_empty());
    }

    #[test]
    fn invalidation_map_marks_relative_and_nth_dependencies_conservative() {
        let map = test_invalidation_map(
            r#"
                .card:has(> .badge) { color:red }
                .row:has(+ .notice) .label { color:red }
                .item:nth-child(2n of .eligible) { color:red }
                .typed:nth-of-type(odd) { color:red }
                .trigger ~ .branch .leaf { color:red }
                :root { color:red }
                a:link, a:visited, :target { color:red }
            "#,
        );

        assert!(map.requires_conservative_invalidation());
        assert!(map.conservative_rule_orders().len() >= 3);
        assert!(!map.has_unkeyed_relational_rules());
        assert!(dependencies_reach(
            map.class_dependencies("badge"),
            InvalidationReaches::CONSERVATIVE,
        ));
        assert!(dependencies_reach(
            map.class_dependencies("notice"),
            InvalidationReaches::CONSERVATIVE,
        ));
        assert!(dependencies_reach(
            map.class_dependencies("eligible"),
            InvalidationReaches::CONSERVATIVE,
        ));
        assert!(!map.state_dependencies("nth-child").is_empty());
        assert!(!map.state_dependencies("nth-of-type").is_empty());
        let trigger = map.class_dependencies("trigger");
        assert!(dependencies_reach(
            trigger,
            InvalidationReaches::SIBLINGS,
        ));
        assert!(dependencies_reach(
            trigger,
            InvalidationReaches::CONSERVATIVE,
        ));
        assert!(dependencies_reach(
            map.class_dependencies("branch"),
            InvalidationReaches::DESCENDANTS,
        ));
        for state in ["root", "link", "visited", "target"] {
            assert!(
                !map.state_dependencies(state).is_empty(),
                "missing conservative state dependency for :{state}",
            );
        }
    }

    #[test]
    fn invalidation_map_distinguishes_keyed_and_unkeyed_relational_subjects() {
        let keyed = test_invalidation_map(
            ".card:has(> .badge), .row:has(.icon[data-live]), .copy:has(> span), .choice:has(:is(.yes,button)) { color:red }",
        );
        assert!(!keyed.has_unkeyed_relational_rules());
        assert!(keyed
            .class_dependencies("badge")
            .iter()
            .any(|dependency| keyed.is_relational_rule(dependency.rule_order)));
        assert!(keyed
            .local_name_dependencies("span")
            .iter()
            .any(|dependency| keyed.is_relational_rule(dependency.rule_order)));

        for selector in [
            ".card:has(> *){color:red}",
            ".card:has(:is(.badge,*)){color:red}",
            ".card:has(:empty){color:red}",
        ] {
            assert!(
                test_invalidation_map(selector).has_unkeyed_relational_rules(),
                "{selector}"
            );
        }
    }

    #[test]
    fn relational_invalidation_records_anchor_reach_and_tree_side_effects() {
        let map = test_invalidation_map(
            r#"
                .host:has(.signal) .out { color:red }
                .row:has(+ :is(.notice,.warning)) { color:red }
                .card:has(.item:first-child) { color:red }
                .shell:has(.label:empty) { color:red }
                .anchor:has(.signal) ~ .panel .leaf { color:red }
            "#,
        );
        let entries = map.relational_invalidations();
        assert_eq!(entries.len(), 5);
        assert!(entries[0]
            .anchor_reaches
            .contains(InvalidationReaches::DESCENDANTS));
        assert!(!entries[0].unkeyed_subject);
        assert!(entries[1].sibling_side_effect);
        assert!(!entries[1].unkeyed_subject);
        assert!(entries[2].structural_side_effect);
        assert!(!entries[2].text_side_effect);
        assert!(entries[3].structural_side_effect);
        assert!(entries[3].text_side_effect);
        assert!(entries[4].unrepresentable_outer_path);

        let tree = obscura_dom::parse_html(
            "<section id=host class=host></section><section id=other class=other></section>",
        );
        assert!(entries[0].anchor_may_match(
            &tree,
            tree.get_element_by_id("host").unwrap(),
        ));
        assert!(!entries[0].anchor_may_match(
            &tree,
            tree.get_element_by_id("other").unwrap(),
        ));
    }

    #[test]
    fn relational_anchor_filter_never_unqualifies_namespaced_attributes() {
        for compound in [
            "[xlink|href]:has(.signal)",
            "[*|href]:has(.signal)",
            "[|href]:has(.signal)",
        ] {
            assert_eq!(relational_anchor_key(compound), None, "{compound}");
        }
        assert_eq!(
            relational_anchor_key("[xlink|href].host:has(.signal)"),
            Some(RelationalSelectorKey::Class("host".into())),
        );
    }

    #[test]
    fn invalidation_map_includes_generated_content_host_dependencies() {
        let map = test_invalidation_map(
            r#"
                .toolbar[data-open] > .button:hover::before { content:"x" }
                #status::after { content:"ok" }
            "#,
        );

        assert!(dependencies_reach(
            map.class_dependencies("toolbar"),
            InvalidationReaches::DESCENDANTS,
        ));
        assert!(dependencies_reach(
            map.attribute_dependencies("data-open"),
            InvalidationReaches::DESCENDANTS,
        ));
        assert!(dependencies_reach(
            map.class_dependencies("button"),
            InvalidationReaches::SELF,
        ));
        assert!(dependencies_reach(
            map.state_dependencies("hover"),
            InvalidationReaches::SELF,
        ));
        assert!(dependencies_reach(
            map.id_dependencies("status"),
            InvalidationReaches::SELF,
        ));
    }

    fn condition_arena_root() -> Vec<ContainerConditionNode> {
        vec![ContainerConditionNode {
            parent: ContainerConditionId::NONE,
            alternatives: Vec::new(),
        }]
    }

    fn cascade_layer_target(
        sources: &[&str],
        inline_css: Option<&str>,
    ) -> (LayoutStyle, HashMap<String, String>, Option<LayoutStyle>) {
        let tree = obscura_dom::parse_html(r#"<div id="target" class="target"></div>"#);
        let target = tree.get_element_by_id("target").unwrap();
        let source_strings = sources
            .iter()
            .map(|source| (*source).to_string())
            .collect::<Vec<_>>();
        let sheet = Stylesheet::parse(&tree, &source_strings);
        let mut matcher = tree.matcher();
        let mut style = LayoutStyle::default();
        let effective = sheet
            .apply(
                &tree,
                &mut matcher,
                target,
                Some("target"),
                &["target".to_string()],
                "div",
                &mut style,
                &HashMap::new(),
                inline_css,
            )
            .unwrap_or_default();
        let before = sheet
            .pseudo_styles(&tree, &mut matcher, target, &effective, &style)
            .0;
        (style, effective, before)
    }

    #[test]
    #[ignore = "release-only cascade microbenchmark"]
    fn benchmark_sparse_cascade_hot_path() {
        use std::hint::black_box;
        use std::time::Instant;

        const RULE_COUNT: usize = 96;
        const ITERATIONS: usize = 6_000;

        let tree = obscura_dom::parse_html(r#"<div id="target" class="target"></div>"#);
        let target = tree.get_element_by_id("target").unwrap();
        let css = (0..RULE_COUNT)
            .map(|index| {
                format!(
                    ".target {{ width:{}px; margin-left:{}px; color:rgb({}, {}, {}) }}",
                    index + 1,
                    index % 17,
                    index % 255,
                    (index * 3) % 255,
                    (index * 7) % 255,
                )
            })
            .collect::<String>();
        let sheet = Stylesheet::parse(&tree, &[css]);
        let mut matcher = tree.matcher();
        let classes = vec!["target".to_string()];
        let parent_props = (0..32)
            .map(|index| (format!("--inherited-{index}"), format!("{}px", index + 1)))
            .collect::<HashMap<_, _>>();

        let started = Instant::now();
        for _ in 0..ITERATIONS {
            let mut style = LayoutStyle::default();
            let effective = sheet.apply(
                &tree,
                &mut matcher,
                target,
                Some("target"),
                &classes,
                "div",
                &mut style,
                &parent_props,
                None,
            );
            black_box((&style, effective));
        }
        let elapsed = started.elapsed();
        eprintln!(
            "sparse cascade: {ITERATIONS} iterations, {} ns/apply ({elapsed:?})",
            elapsed.as_nanos() / ITERATIONS as u128,
        );
    }

    #[test]
    fn declaration_stream_flags_guard_hot_passes_and_borrow_plain_rules() {
        let plain = "width:12px;color:red;";
        let plain_flags = declaration_stream_flags(plain);
        assert_eq!(plain_flags, DeclarationStreamFlags::default());
        assert!(matches!(
            substitute_declarations(plain, &HashMap::new(), plain_flags.has_var),
            Cow::Borrowed(value) if value == plain
        ));

        let dynamic = "--tone:dark;color-scheme:light dark;animation-name:pulse;color:var(--tone);";
        let flags = declaration_stream_flags(dynamic);
        assert!(flags.has_custom_properties);
        assert!(flags.has_var);
        assert!(flags.has_color_scheme);
        assert!(flags.has_animation);
        let props = HashMap::from([("--tone".to_string(), "rebeccapurple".to_string())]);
        let expanded = substitute_declarations(dynamic, &props, flags.has_var);
        assert!(matches!(&expanded, Cow::Owned(_)));
        assert!(expanded.contains("color:rebeccapurple;"));

        let similarly_named = declaration_stream_flags(
            "my-color-scheme:dark;animationish:fade;background:variety(red);",
        );
        assert!(!similarly_named.has_var);
        assert!(!similarly_named.has_color_scheme);
        assert!(!similarly_named.has_animation);
    }

    #[test]
    fn root_rules_use_the_document_element_bucket_without_losing_is_arms() {
        let tree = obscura_dom::parse_html(
            r#"<html><body><div id="card" class="card"></div></body></html>"#,
        );
        let sheet = Stylesheet::parse(
            &tree,
            &[r#"
                :root { width:321px }
                :is(:root, .card) { height:123px }
            "#
            .to_string()],
        );
        let (_, _, class_keys, _, _, universal) = sheet.debug_stats();
        assert_eq!(class_keys, 1);
        assert_eq!(
            universal, 0,
            ":root must not remain in the universal bucket"
        );

        let root = tree.query_selector("html").unwrap().unwrap();
        let body = tree.query_selector("body").unwrap().unwrap();
        let card = tree.get_element_by_id("card").unwrap();
        let mut matcher = tree.matcher();
        let mut root_style = LayoutStyle::default();
        sheet.apply(
            &tree,
            &mut matcher,
            root,
            None,
            &[],
            "html",
            &mut root_style,
            &HashMap::new(),
            None,
        );
        assert_eq!(root_style.width, crate::Dimension::Px(321.0));
        assert_eq!(root_style.height, crate::Dimension::Px(123.0));

        let mut body_style = LayoutStyle::default();
        sheet.apply(
            &tree,
            &mut matcher,
            body,
            None,
            &[],
            "body",
            &mut body_style,
            &HashMap::new(),
            None,
        );
        assert_ne!(body_style.height, crate::Dimension::Px(123.0));

        let mut card_style = LayoutStyle::default();
        sheet.apply(
            &tree,
            &mut matcher,
            card,
            Some("card"),
            &["card".to_string()],
            "div",
            &mut card_style,
            &HashMap::new(),
            None,
        );
        assert_eq!(card_style.height, crate::Dimension::Px(123.0));
    }

    #[test]
    fn pseudo_rules_use_subject_buckets_and_dense_dedup() {
        let tree = obscura_dom::parse_html(
            r#"<div id="both" class="alpha" data-kind="both"></div>
                <div id="attribute" data-kind="attribute"></div>"#,
        );
        let sheet = Stylesheet::parse(
            &tree,
            &[r#"
                :is(.alpha, [data-kind])::before { content:"indexed" }
                .missing::before { content:"wrong" }
            "#
            .to_string()],
        );

        assert_eq!(sheet.before_rules.rules.len(), 2);
        assert_eq!(sheet.before_rules.by_class.get("alpha").unwrap(), &[0]);
        assert_eq!(
            sheet.before_rules.by_attribute.get("data-kind").unwrap(),
            &[0]
        );
        assert_eq!(sheet.before_rules.rules[0].candidate_slot, 0);
        assert_eq!(sheet.before_rules.candidate_slot_count, 1);

        for id in ["both", "attribute"] {
            let target = tree.get_element_by_id(id).unwrap();
            let before = sheet
                .pseudo_styles(
                    &tree,
                    &mut tree.matcher(),
                    target,
                    &HashMap::new(),
                    &LayoutStyle::default(),
                )
                .0
                .expect("the indexed pseudo selector should match");
            assert_eq!(before.before_content.as_deref(), Some("indexed"));
        }
    }

    #[test]
    fn pseudo_candidate_bucket_does_not_replace_full_selector_matching() {
        let tree = obscura_dom::parse_html(r#"<div id="target" class="alpha blocked"></div>"#);
        let target = tree.get_element_by_id("target").unwrap();
        let sheet = Stylesheet::parse(
            &tree,
            &[r#"
                .alpha::before { content:"base" }
                .alpha:not(.blocked)::before { content:"wrong" }
            "#
            .to_string()],
        );
        let before = sheet
            .pseudo_styles(
                &tree,
                &mut tree.matcher(),
                target,
                &HashMap::new(),
                &LayoutStyle::default(),
            )
            .0
            .expect("the base pseudo selector should match");
        assert_eq!(before.before_content.as_deref(), Some("base"));
    }

    #[test]
    fn disjoint_functional_subjects_use_all_buckets_and_one_dense_slot() {
        let tree = obscura_dom::parse_html(
            r#"<div id="target" class="alpha" data-kind="both"></div><div data-kind="attribute"></div>"#,
        );
        let target = tree.get_element_by_id("target").unwrap();
        let sheet = Stylesheet::parse(
            &tree,
            &[":is(.alpha, [data-kind]) { width:47px }".to_string()],
        );

        assert!(sheet.universal.is_empty());
        assert_eq!(sheet.by_class.get("alpha").unwrap(), &[0]);
        assert_eq!(sheet.by_attribute.get("data-kind").unwrap(), &[0]);
        assert_eq!(sheet.rules[0].candidate_slot, 0);
        assert_eq!(sheet.candidate_slot_count, 1);

        let mut matcher = tree.matcher();
        let mut style = LayoutStyle::default();
        sheet.apply(
            &tree,
            &mut matcher,
            target,
            Some("target"),
            &["alpha".to_string()],
            "div",
            &mut style,
            &HashMap::new(),
            None,
        );
        assert_eq!(style.width, crate::Dimension::Px(47.0));
    }

    #[test]
    fn layer_order_statement_beats_block_source_order() {
        let (style, _, _) = cascade_layer_target(
            &[r#"
                @layer reset, components, utilities;
                @layer utilities { #target { width:320px } }
                @layer components { #target { width:100% } }
            "#],
            None,
        );
        assert_eq!(style.width, crate::Dimension::Px(320.0));
    }

    #[test]
    fn unlayered_normal_beats_layered_higher_specificity() {
        let (style, _, _) = cascade_layer_target(
            &[r#"
                @layer utilities { #target { width:320px } }
                .target { width:200px }
            "#],
            None,
        );
        assert_eq!(style.width, crate::Dimension::Px(200.0));
    }

    #[test]
    fn important_layer_order_is_reversed_and_beats_unlayered() {
        let css = r#"
            @layer reset, overrides;
            @layer overrides { #target { width:400px !important } }
            @layer reset { .target { width:100px !important } }
            #target { width:300px !important }
        "#;
        let (style, _, _) = cascade_layer_target(&[css], Some("width:500px"));
        assert_eq!(
            style.width,
            crate::Dimension::Px(100.0),
            "the earliest layered important declaration beats later layers, unlayered author rules, and normal inline style"
        );
        let (inline_important, _, _) = cascade_layer_target(&[css], Some("width:500px !important"));
        assert_eq!(
            inline_important.width,
            crate::Dimension::Px(500.0),
            "important inline style remains strongest within the author origin"
        );
    }

    #[test]
    fn sparse_priority_streams_preserve_normal_and_important_order() {
        let (style, _, _) = cascade_layer_target(
            &[r#"
                @layer first, second;
                @layer second {
                    #target { width:20px }
                    #target { height:40px !important }
                }
                @layer first {
                    .target { width:10px }
                    .target { height:30px !important }
                }
                .target { width:50px }
            "#],
            None,
        );
        assert_eq!(style.width, crate::Dimension::Px(50.0));
        assert_eq!(style.height, crate::Dimension::Px(30.0));
    }

    #[test]
    fn inherited_custom_property_fast_path_reuses_parent_map() {
        let tree = obscura_dom::parse_html(r#"<div id="target" class="target"></div>"#);
        let target = tree.get_element_by_id("target").unwrap();
        let sheet = Stylesheet::parse(
            &tree,
            &[r#"
                .target {
                    width:var(--inherited-size);
                    background-image:url("/asset--content-hash.png");
                }
            "#
            .to_string()],
        );
        let parent_props = HashMap::from([("--inherited-size".to_string(), "37px".to_string())]);
        let mut matcher = tree.matcher();
        let mut style = LayoutStyle::default();
        let effective = sheet.apply(
            &tree,
            &mut matcher,
            target,
            Some("target"),
            &["target".to_string()],
            "div",
            &mut style,
            &parent_props,
            None,
        );

        assert_eq!(style.width, crate::Dimension::Px(37.0));
        assert!(
            effective.is_none(),
            "an inherited var() use and `--` inside a value must not allocate a new property map"
        );
    }

    #[test]
    fn layer_order_applies_to_custom_properties_and_pseudos() {
        let (style, props, before) = cascade_layer_target(
            &[r#"
                @layer base, theme;
                @layer theme {
                    #target { --normal-size:30px; --critical-size:31px !important }
                    #target::before { content:"theme"; width:30px }
                }
                @layer base {
                    #target { --normal-size:10px; --critical-size:11px !important }
                    #target::before { content:"base" !important; width:10px }
                }
                #target { width:var(--normal-size); height:var(--critical-size) }
            "#],
            None,
        );
        assert_eq!(props.get("--normal-size").map(String::as_str), Some("30px"));
        assert_eq!(
            props.get("--critical-size").map(String::as_str),
            Some("11px")
        );
        assert_eq!(style.width, crate::Dimension::Px(30.0));
        assert_eq!(style.height, crate::Dimension::Px(11.0));
        let before = before.expect("the layered ::before content should materialize");
        assert_eq!(before.before_content.as_deref(), Some("base"));
        assert_eq!(before.width, crate::Dimension::Px(30.0));
    }

    #[test]
    fn nested_and_anonymous_layers_keep_parent_direct_precedence() {
        let (style, _, _) = cascade_layer_target(
            &[r#"
                @layer framework {
                    @layer reset, components;
                    #target { width:90px; height:90px !important }
                    @layer components {
                        #target { width:30px; height:30px !important }
                    }
                    @layer reset {
                        #target { width:10px; height:10px !important }
                    }
                    @layer {
                        #target { width:60px; height:60px !important }
                    }
                }
            "#],
            None,
        );
        assert_eq!(
            style.width,
            crate::Dimension::Px(90.0),
            "direct normal declarations are the parent layer's implicit final sub-layer"
        );
        assert_eq!(
            style.height,
            crate::Dimension::Px(10.0),
            "nested important order reverses, making the first named child strongest"
        );
    }

    #[test]
    fn layer_registry_spans_multiple_stylesheet_sources() {
        let (style, _, _) = cascade_layer_target(
            &[
                "@layer base, utilities; @layer utilities { #target { width:320px } }",
                "@layer base { #target { width:100% } }",
            ],
            None,
        );
        assert_eq!(style.width, crate::Dimension::Px(320.0));
    }

    #[test]
    fn generated_content_resolves_host_attributes_and_resets() {
        let tree =
            obscura_dom::parse_html(r#"<button id="cta" data-label="Get Started"></button>"#);
        let cta = tree.query_selector("#cta").unwrap().unwrap();
        assert_eq!(
            extract_content("content:attr(data-label)", &tree, cta),
            Some(Some(vec![crate::GeneratedContentItem::Text(
                "Get Started".to_string()
            )]))
        );
        assert_eq!(
            extract_content(r#"content:"fallback";content:none"#, &tree, cta),
            Some(None)
        );
    }

    #[test]
    fn generated_content_parses_counter_items_and_styles() {
        let tree = obscura_dom::parse_html(r#"<span id="line"></span>"#);
        let line = tree.query_selector("#line").unwrap().unwrap();
        assert_eq!(
            extract_content(
                r#"content:"[" counters(section, ".", upper-roman) "] " counter(line)"#,
                &tree,
                line,
            ),
            Some(Some(vec![
                crate::GeneratedContentItem::Text("[".to_string()),
                crate::GeneratedContentItem::Counters {
                    name: "section".to_string(),
                    separator: ".".to_string(),
                    style: crate::GeneratedCounterStyle::UpperRoman,
                },
                crate::GeneratedContentItem::Text("] ".to_string()),
                crate::GeneratedContentItem::Counter {
                    name: "line".to_string(),
                    style: crate::GeneratedCounterStyle::Decimal,
                },
            ]))
        );
        assert_eq!(
            format_counter_value(27, crate::GeneratedCounterStyle::LowerAlpha),
            "aa"
        );
        assert_eq!(
            format_counter_value(14, crate::GeneratedCounterStyle::UpperRoman),
            "XIV"
        );
        assert_eq!(
            format_counter_value(-4, crate::GeneratedCounterStyle::DecimalLeadingZero),
            "-04"
        );
    }

    #[test]
    fn keyframes_retain_every_animation_offset() {
        let css = r#"
            @keyframes dismiss {
                from { opacity: 1; visibility: visible; }
                50% { opacity: .5; }
                to { opacity: 0; visibility: hidden; }
            }
            @-webkit-keyframes slide {
                0% { transform: translateX(0); }
                100% { transform: translateX(20px); }
            }
        "#;
        let keyframes: HashMap<_, _> = extract_keyframes(css).into_iter().collect();
        let dismiss = normalized_keyframe_offsets(&keyframes["dismiss"].stops);
        assert_eq!(
            dismiss
                .iter()
                .map(|(offset, _)| *offset)
                .collect::<Vec<_>>(),
            [0.0, 0.5, 1.0]
        );
        assert!(dismiss[0].1.declarations.contains("opacity: 1"));
        assert!(dismiss[2].1.declarations.contains("visibility: hidden"));
        let slide = normalized_keyframe_offsets(&keyframes["slide"].stops);
        assert_eq!(
            slide.iter().map(|(offset, _)| *offset).collect::<Vec<_>>(),
            [0.0, 1.0]
        );
        assert!(slide[1].1.declarations.contains("translateX(20px)"));
    }

    fn sampled_animation_style(css: &str, sample_ms: f32, target_id: &str) -> LayoutStyle {
        let tree = obscura_dom::parse_html(&format!(r#"<div id="{target_id}"></div>"#));
        let target = tree.get_element_by_id(target_id).unwrap();
        let sheet = Stylesheet::parse_for_viewport_at_animation_time(
            &tree,
            &[css.to_string()],
            (1280.0, 720.0),
            crate::AnimationSampleTime {
                milliseconds: sample_ms,
            },
        );
        let node = tree.get_node(target).unwrap();
        let element = node.as_element().unwrap();
        let mut matcher = tree.matcher();
        let mut style = LayoutStyle::default();
        sheet.apply(
            &tree,
            &mut matcher,
            target,
            node.get_attribute("id"),
            &[],
            element.local.as_ref(),
            &mut style,
            &HashMap::new(),
            None,
        );
        style
    }

    fn sampled_fade(extra: &str, sample_ms: f32) -> f32 {
        let css = format!(
            r#"
                @keyframes fade {{ from {{ opacity:0 }} to {{ opacity:1 }} }}
                #target {{
                    opacity:.4;
                    animation:fade 1s linear infinite;
                    {extra}
                }}
            "#
        );
        sampled_animation_style(&css, sample_ms, "target")
            .opacity
            .unwrap_or(1.0)
    }

    fn assert_opacity(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 0.0001,
            "expected opacity {expected}, got {actual}"
        );
    }

    #[test]
    fn animation_effect_impact_separates_paint_from_geometry_tracks() {
        let sampled = |declarations: &str| {
            sampled_animation_style(
                &format!(
                    "@keyframes effect {{ from {{ {declarations} }} to {{ {declarations} }} }}\
                     #target {{ animation:effect 1s linear infinite }}"
                ),
                100.0,
                "target",
            )
            .animation_effect_impact
        };

        assert_eq!(
            sampled("opacity:.5;color:red;background-color:blue;border-color:green;\
                     background-position:20% 30%;visibility:hidden"),
            crate::AnimationEffectImpact::Paint,
        );
        for declarations in [
            "transform:translateX(1px)",
            "translate:1px 2px",
            "rotate:10deg",
            "scale:2",
            "width:20px",
            "height:20px",
            "inset:1px",
            "margin:1px",
            "padding:1px",
            "gap:1px",
            "flex-basis:20px",
        ] {
            assert_eq!(
                sampled(declarations),
                crate::AnimationEffectImpact::Geometry,
                "{declarations}",
            );
        }
        assert_eq!(
            sampled("--unsupported:1"),
            crate::AnimationEffectImpact::None,
        );
    }

    #[test]
    fn animation_samples_geometry_transform_and_paint_properties_at_t0() {
        let css = r#"
            @keyframes demo {
                from {
                    transform:translateX(100px);
                    translate:20px 30px;
                    rotate:10deg;
                    scale:2 3;
                    width:300px;
                    height:80px;
                    min-width:120px;
                    max-height:90px;
                    inset:1px 2px 3px 4px;
                    margin:5px 6px 7px 8px;
                    padding:9px 10px 11px 12px;
                    gap:13px 14px;
                    flex-basis:150px;
                    color:rgb(10,20,30);
                    background:red;
                    border-color:blue green yellow black;
                    background-position:25% 40%;
                    visibility:hidden;
                    opacity:.25;
                }
                to { opacity:.75 }
            }
            #target { width:50px; animation:demo 10s both }
        "#;
        let style = sampled_animation_style(css, 0.0, "target");
        assert_eq!(style.width, crate::Dimension::Px(300.0));
        assert_eq!(style.height, crate::Dimension::Px(80.0));
        assert_eq!(style.min_width, crate::Dimension::Px(120.0));
        assert_eq!(style.max_height, crate::Dimension::Px(90.0));
        assert_eq!(style.inset[3], Some(crate::Dimension::Px(4.0)));
        assert_eq!(style.margin.left, 8.0);
        assert_eq!(style.padding.bottom, 11.0);
        assert_eq!(style.row_gap, Some(13.0));
        assert_eq!(style.column_gap, Some(14.0));
        assert_eq!(style.flex_basis, crate::Dimension::Px(150.0));
        assert_eq!(style.color, Some([10, 20, 30, 255]));
        assert_eq!(style.background_color, Some([255, 0, 0, 255]));
        assert_eq!(style.border_model.colors.top, Some([0, 0, 255, 255]));
        assert_eq!(style.border_model.colors.right, Some([0, 128, 0, 255]));
        assert_eq!(style.visibility_hidden, Some(true));
        assert_opacity(style.opacity.unwrap(), 0.25);
        assert!(matches!(
            style.transform_ops.as_slice(),
            [crate::TransformOp::Translate(x, y)]
                if x.value == crate::Dimension::Px(100.0)
                    && y.value == crate::Dimension::Px(0.0)
        ));
        assert_eq!(
            style.individual_translate,
            Some((crate::Dimension::Px(20.0), crate::Dimension::Px(30.0)))
        );
        assert_eq!(style.individual_rotate, Some(10.0));
        assert_eq!(style.individual_scale, Some((2.0, 3.0)));
    }

    #[test]
    fn animation_t0_geometry_reaches_the_real_layout_pass() {
        let tree = obscura_dom::parse_html(
            r#"
                <style>
                    html,body { margin:0 }
                    #row { display:flex }
                    @keyframes size {
                        from { width:300px; height:20px }
                        to { width:400px; height:40px }
                    }
                    #target { width:50px; animation:size 10s both }
                    #sibling { width:10px; height:10px }
                </style>
                <div id="row"><div id="target"></div><div id="sibling"></div></div>
            "#,
        );
        let target = tree.get_element_by_id("target").unwrap();
        let sibling = tree.get_element_by_id("sibling").unwrap();
        let laid = crate::dom::layout_dom(&tree, (800.0, 600.0));
        assert_eq!(laid.rects[&target].width, 300.0);
        assert_eq!(laid.rects[&target].height, 20.0);
        assert_eq!(laid.rects[&sibling].x, 300.0);
    }

    #[test]
    fn animation_sparse_tracks_ignore_unrelated_intermediate_keyframes() {
        let css = r#"
            @keyframes sparse {
                from { width:100px; opacity:0; background-color:red }
                50% { opacity:1 }
                to { width:300px; opacity:0; background-color:blue }
            }
            #target { animation:sparse 1s linear both }
        "#;
        let style = sampled_animation_style(css, 500.0, "target");
        assert_eq!(style.width, crate::Dimension::Px(200.0));
        assert_opacity(style.opacity.unwrap(), 1.0);
        assert_eq!(style.background_color, Some([128, 0, 128, 255]));

        let implicit = r#"
            @keyframes middle { 50% { width:150px } }
            #target { width:50px; animation:middle 1s linear both }
        "#;
        assert_eq!(
            sampled_animation_style(implicit, 250.0, "target").width,
            crate::Dimension::Px(100.0)
        );
        assert_eq!(
            sampled_animation_style(implicit, 750.0, "target").width,
            crate::Dimension::Px(100.0)
        );
    }

    #[test]
    fn keyframe_duplicate_offsets_merge_per_property_and_ignore_important() {
        let css = r#"
            @keyframes merged {
                0% { width:100px; color:red; opacity:.1 }
                from { width:999px !important; opacity:.8 }
            }
            #target { width:40px; animation:merged 1s both }
        "#;
        let style = sampled_animation_style(css, 0.0, "target");
        assert_eq!(style.width, crate::Dimension::Px(100.0));
        assert_eq!(style.color, Some([255, 0, 0, 255]));
        assert_opacity(style.opacity.unwrap(), 0.8);
    }

    #[test]
    fn author_important_overrides_every_sampled_animation_property() {
        let css = r#"
            @keyframes forced { from, to { width:300px; background-color:red } }
            #target {
                animation:forced 1s both;
                width:80px !important;
                background-color:blue !important;
            }
        "#;
        let style = sampled_animation_style(css, 0.0, "target");
        assert_eq!(style.width, crate::Dimension::Px(80.0));
        assert_eq!(style.background_color, Some([0, 0, 255, 255]));
    }

    #[test]
    fn waapi_samples_opacity_and_transform_without_rewriting_inline_cascade() {
        let tree = obscura_dom::parse_html(r#"<div id="target" style="opacity:.2"></div>"#);
        let target = tree.get_element_by_id("target").unwrap();
        let sheet = Stylesheet::parse_for_viewport(&tree, &[], (800.0, 600.0));
        let mut timeline = crate::AnimationTimelineState::default();
        timeline.register_waapi(crate::WaapiAnimation {
            id: 1,
            node: target,
            keyframes: vec![
                crate::WaapiKeyframe { offset: 0.0, opacity: Some(0.2), transform: Some("translateX(0px)".into()) },
                crate::WaapiKeyframe { offset: 1.0, opacity: Some(1.0), transform: Some("translateX(100px)".into()) },
            ],
            timing: crate::AnimationTiming {
                duration_ms: 100.0,
                fill_mode: crate::AnimationFillMode::Both,
                ..Default::default()
            },
            easing: None,
            linear_easing: None,
            start_time_ms: 0.0,
            hold_time_ms: Some(50.0),
            play_state: crate::WaapiPlayState::Paused,
        });
        let node = tree.get_node(target).unwrap();
        let element = node.as_element().unwrap();
        let mut matcher = tree.matcher();
        let mut style = LayoutStyle::default();
        sheet.apply_at_animation_time(
            &tree,
            &mut matcher,
            target,
            node.get_attribute("id"),
            &[],
            element.local.as_ref(),
            &mut style,
            &HashMap::new(),
            node.get_attribute("style"),
            crate::AnimationSample::document(50.0),
            &mut timeline,
        );
        assert_opacity(style.opacity.unwrap(), 0.6);
        assert!(matches!(style.transform_ops.as_slice(),
            [crate::TransformOp::Translate(x, _)] if x.value == crate::Dimension::Px(50.0)));

        let important_tree = obscura_dom::parse_html(
            r#"<div id="target" style="opacity:.2 !important"></div>"#,
        );
        let target = important_tree.get_element_by_id("target").unwrap();
        let sheet = Stylesheet::parse_for_viewport(&important_tree, &[], (800.0, 600.0));
        let mut timeline = crate::AnimationTimelineState::default();
        timeline.register_waapi(crate::WaapiAnimation {
            id: 2,
            node: target,
            keyframes: vec![crate::WaapiKeyframe { offset: 1.0, opacity: Some(1.0), transform: None }],
            timing: crate::AnimationTiming { duration_ms: 100.0, fill_mode: crate::AnimationFillMode::Both, ..Default::default() },
            easing: None,
            linear_easing: None,
            start_time_ms: 0.0,
            hold_time_ms: Some(100.0),
            play_state: crate::WaapiPlayState::Finished,
        });
        let node = important_tree.get_node(target).unwrap();
        let mut matcher = important_tree.matcher();
        let mut style = LayoutStyle::default();
        sheet.apply_at_animation_time(
            &important_tree, &mut matcher, target, node.get_attribute("id"), &[], "div",
            &mut style, &HashMap::new(), node.get_attribute("style"),
            crate::AnimationSample::document(100.0), &mut timeline,
        );
        assert_opacity(style.opacity.unwrap(), 0.2);
    }

    #[test]
    fn retained_waapi_transform_resampling_uses_underlying_and_respects_important() {
        let tree = obscura_dom::parse_html(
            r#"<div id="target" style="transform:translateX(10px)"></div>"#,
        );
        let target = tree.get_element_by_id("target").unwrap();
        let sheet = Stylesheet::parse_for_viewport(&tree, &[], (800.0, 600.0));
        let make_timeline = || {
            let mut timeline = crate::AnimationTimelineState::default();
            timeline.register_waapi(crate::WaapiAnimation {
                id: 1,
                node: target,
                keyframes: vec![crate::WaapiKeyframe {
                    offset: 0.5,
                    opacity: None,
                    transform: Some("translateX(50px)".into()),
                }],
                timing: crate::AnimationTiming {
                    duration_ms: 1_000.0,
                    fill_mode: crate::AnimationFillMode::Both,
                    ..Default::default()
                },
                easing: None,
                linear_easing: None,
                start_time_ms: 0.0,
                hold_time_ms: None,
                play_state: crate::WaapiPlayState::Running,
            });
            timeline
        };
        let apply = |time, timeline: &mut crate::AnimationTimelineState| {
            let node = tree.get_node(target).unwrap();
            let mut matcher = tree.matcher();
            let mut style = LayoutStyle::default();
            sheet.apply_at_animation_time(
                &tree,
                &mut matcher,
                target,
                node.get_attribute("id"),
                &[],
                "div",
                &mut style,
                &HashMap::new(),
                node.get_attribute("style"),
                crate::AnimationSample::document(time),
                timeline,
            );
            style
        };
        let mut timeline = make_timeline();
        let retained = apply(0.0, &mut timeline);
        for time in [500.0, 750.0] {
            let resampled = resample_visual_waapi(
                &timeline,
                target,
                &retained,
                crate::AnimationSample::document(time),
            )
            .expect("sparse transform target is eligible")
            .transform_ops;
            let mut fresh_timeline = make_timeline();
            let fresh = apply(time, &mut fresh_timeline);
            assert_eq!(format!("{resampled:?}"), format!("{:?}", fresh.transform_ops));
        }

        let important_tree = obscura_dom::parse_html(
            r#"<div id="target" style="transform:translateX(10px) !important"></div>"#,
        );
        let important_target = important_tree.get_element_by_id("target").unwrap();
        let important_sheet =
            Stylesheet::parse_for_viewport(&important_tree, &[], (800.0, 600.0));
        let mut important_timeline = crate::AnimationTimelineState::default();
        important_timeline.register_waapi(crate::WaapiAnimation {
            id: 2,
            node: important_target,
            keyframes: vec![crate::WaapiKeyframe {
                offset: 1.0,
                opacity: None,
                transform: Some("translateX(100px)".into()),
            }],
            timing: crate::AnimationTiming {
                duration_ms: 1_000.0,
                fill_mode: crate::AnimationFillMode::Both,
                ..Default::default()
            },
            easing: None,
            linear_easing: None,
            start_time_ms: 0.0,
            hold_time_ms: None,
            play_state: crate::WaapiPlayState::Running,
        });
        let node = important_tree.get_node(important_target).unwrap();
        let mut matcher = important_tree.matcher();
        let mut important_style = LayoutStyle::default();
        important_sheet.apply_at_animation_time(
            &important_tree,
            &mut matcher,
            important_target,
            node.get_attribute("id"),
            &[],
            "div",
            &mut important_style,
            &HashMap::new(),
            node.get_attribute("style"),
            crate::AnimationSample::document(0.0),
            &mut important_timeline,
        );
        assert!(resample_visual_waapi(
            &important_timeline,
            important_target,
            &important_style,
            crate::AnimationSample::document(500.0),
        )
        .is_none());
    }

    #[test]
    fn conditional_keyframes_and_empty_later_definitions_follow_browser_selection() {
        let css = r#"
            @keyframes chosen { from, to { width:222px } }
            @media (max-width:100px) {
                @keyframes chosen { from, to { width:111px } }
            }
            @supports (unknown-parity-property:value) {
                @keyframes chosen { from, to { width:123px } }
            }
            #target { width:50px; animation:chosen 1s both }
        "#;
        assert_eq!(
            sampled_animation_style(css, 0.0, "target").width,
            crate::Dimension::Px(222.0)
        );

        let empty = r#"
            @keyframes cleared { from, to { width:300px } }
            @keyframes cleared {}
            #target { width:50px; animation:cleared 1s both }
        "#;
        assert_eq!(
            sampled_animation_style(empty, 0.0, "target").width,
            crate::Dimension::Px(50.0)
        );
    }

    #[test]
    fn keyframe_names_follow_cascade_layer_order_and_standard_prefix_priority() {
        let unlayered = r#"
            @keyframes chosen { from, to { width:222px } }
            @layer outer { @keyframes chosen { from, to { width:111px } } }
            #target { animation:chosen 1s both }
        "#;
        assert_eq!(
            sampled_animation_style(unlayered, 0.0, "target").width,
            crate::Dimension::Px(222.0)
        );

        let ordered = r#"
            @layer weak, strong;
            @layer strong { @keyframes chosen { from, to { width:333px } } }
            @layer weak { @keyframes chosen { from, to { width:444px } } }
            #target { animation:chosen 1s both }
        "#;
        assert_eq!(
            sampled_animation_style(ordered, 0.0, "target").width,
            crate::Dimension::Px(333.0)
        );

        let reversed = ordered.replace("@layer weak, strong", "@layer strong, weak");
        assert_eq!(
            sampled_animation_style(&reversed, 0.0, "target").width,
            crate::Dimension::Px(444.0)
        );

        let prefix = r#"
            @keyframes chosen { from, to { width:555px } }
            @-webkit-keyframes chosen { from, to { width:666px } }
            #target { animation:chosen 1s both }
        "#;
        assert_eq!(
            sampled_animation_style(prefix, 0.0, "target").width,
            crate::Dimension::Px(555.0)
        );
    }

    #[test]
    fn animation_timeline_handles_delay_fill_iterations_and_direction_at_t0() {
        assert_opacity(sampled_fade("", 0.0), 0.0);
        assert_opacity(sampled_fade("animation-delay:250ms", 0.0), 0.4);
        assert_opacity(
            sampled_fade("animation-delay:250ms;animation-fill-mode:backwards", 0.0),
            0.0,
        );
        assert_opacity(
            sampled_fade(
                "animation-delay:250ms;animation-fill-mode:backwards;animation-direction:reverse",
                0.0,
            ),
            1.0,
        );
        assert_opacity(sampled_fade("animation-delay:-250ms", 0.0), 0.25);
        assert_opacity(sampled_fade("animation-delay:-1s", 0.0), 0.0);
        assert_opacity(
            sampled_fade("animation-delay:-1s;animation-direction:alternate", 0.0),
            1.0,
        );
        assert_opacity(
            sampled_fade(
                "animation-delay:-1s;animation-iteration-count:1;animation-fill-mode:none",
                0.0,
            ),
            0.4,
        );
        assert_opacity(
            sampled_fade(
                "animation-delay:-1s;animation-iteration-count:1;animation-fill-mode:forwards",
                0.0,
            ),
            1.0,
        );
        assert_opacity(
            sampled_fade(
                "animation-delay:-1s;animation-iteration-count:1;animation-fill-mode:forwards;animation-direction:reverse",
                0.0,
            ),
            0.0,
        );
        assert_opacity(
            sampled_fade(
                "animation-delay:-2s;animation-iteration-count:2;animation-fill-mode:forwards;animation-direction:alternate",
                0.0,
            ),
            0.0,
        );
        assert_opacity(
            sampled_fade(
                "animation-iteration-count:0;animation-fill-mode:forwards",
                0.0,
            ),
            0.0,
        );
        assert_opacity(
            sampled_fade(
                "animation-duration:0s;animation-iteration-count:1;animation-fill-mode:none",
                0.0,
            ),
            0.4,
        );
        assert_opacity(
            sampled_fade(
                "animation-duration:0s;animation-iteration-count:1;animation-fill-mode:forwards",
                0.0,
            ),
            1.0,
        );
        assert_opacity(
            sampled_fade(
                "animation-delay:-2.5s;animation-iteration-count:2.5;animation-fill-mode:forwards",
                0.0,
            ),
            0.5,
        );
    }

    #[test]
    fn animation_calc_delay_pause_and_important_origin_are_deterministic() {
        assert_opacity(
            sampled_fade("--step:.1s;animation-delay:calc(var(--step) * -2.5)", 0.0),
            0.25,
        );
        assert_opacity(sampled_fade("animation-play-state:paused", 500.0), 0.0);
        assert_opacity(sampled_fade("", 500.0), 0.5);
        assert_opacity(
            sampled_fade(
                "animation-delay:-250ms !important;opacity:.8 !important",
                0.0,
            ),
            0.8,
        );
    }

    #[test]
    fn opacity_segments_use_underlying_endpoints_and_exact_later_boundaries() {
        let missing_endpoints = r#"
            @keyframes middle { 50% { opacity:1 } }
            #target { opacity:.4; animation:middle 1s linear 1 both }
        "#;
        assert_opacity(
            sampled_animation_style(missing_endpoints, 250.0, "target")
                .opacity
                .unwrap(),
            0.7,
        );
        assert_opacity(
            sampled_animation_style(missing_endpoints, 750.0, "target")
                .opacity
                .unwrap(),
            0.7,
        );
        let exact_boundary = r#"
            @keyframes peak {
                0% { opacity:0 }
                50% { opacity:1 }
                100% { opacity:0 }
            }
            #target { opacity:.4; animation:peak 1s linear 1 both }
        "#;
        assert_opacity(
            sampled_animation_style(exact_boundary, 500.0, "target")
                .opacity
                .unwrap(),
            1.0,
        );
    }

    #[test]
    fn missing_keyframe_offsets_follow_web_animation_distribution() {
        let stop = |offset| KeyframeStop {
            offset,
            declarations: "opacity:1".to_string(),
            source_order: 0,
        };
        let single = [stop(None)];
        assert_eq!(normalized_keyframe_offsets(&single)[0].0, 1.0);
        let pair = [stop(None), stop(None)];
        assert_eq!(
            normalized_keyframe_offsets(&pair)
                .iter()
                .map(|(offset, _)| *offset)
                .collect::<Vec<_>>(),
            [0.0, 1.0]
        );
        let distributed = [
            stop(Some(0.0)),
            stop(None),
            stop(None),
            stop(Some(0.75)),
            stop(None),
        ];
        assert_eq!(
            normalized_keyframe_offsets(&distributed)
                .iter()
                .map(|(offset, _)| *offset)
                .collect::<Vec<_>>(),
            [0.0, 0.25, 0.5, 0.75, 1.0]
        );
    }

    #[test]
    fn waapi_duplicate_offset_uses_later_value_at_boundary() {
        let track = [(0.0, 0.0), (0.5, 10.0), (0.5, 20.0), (1.0, 30.0)];
        assert_eq!(sample_numeric_waapi_track(&track, -1.0, 0.5), Some(20.0));
        let before = sample_numeric_waapi_track(&track, -1.0, 0.499).unwrap();
        assert!(before < 10.0, "incoming segment must end at the first duplicate");
        let after = sample_numeric_waapi_track(&track, -1.0, 0.501).unwrap();
        assert!(after > 20.0, "outgoing segment must start at the later duplicate");
    }

    #[test]
    fn mozilla_stagger_samples_only_the_delay_zero_frame() {
        let mut html = String::new();
        let mut selectors = String::new();
        for frame in 0..12 {
            html.push_str(&format!(r#"<svg id="frame{frame}" class="frame"></svg>"#));
            selectors.push_str(&format!(
                "#frame{frame}{{animation-delay:calc(var(--base-delay) * {frame})}}"
            ));
        }
        let tree = obscura_dom::parse_html(&html);
        let css = format!(
            r#"
                @keyframes wave {{
                    0%, 8.333% {{ opacity:1 }}
                    8.4%, to {{ opacity:0 }}
                }}
                .frame {{
                    --base-delay:.1s;
                    opacity:0;
                    animation:wave 1.2s linear infinite;
                }}
                {selectors}
            "#
        );
        let sheet = Stylesheet::parse_for_viewport_at_animation_time(
            &tree,
            &[css],
            (1280.0, 720.0),
            crate::AnimationSampleTime::default(),
        );
        for frame in 0..12 {
            let id = format!("frame{frame}");
            let target = tree.get_element_by_id(&id).unwrap();
            let node = tree.get_node(target).unwrap();
            let element = node.as_element().unwrap();
            let classes = vec!["frame".to_string()];
            let mut matcher = tree.matcher();
            let mut style = LayoutStyle::default();
            sheet.apply(
                &tree,
                &mut matcher,
                target,
                node.get_attribute("id"),
                &classes,
                element.local.as_ref(),
                &mut style,
                &HashMap::new(),
                None,
            );
            assert_opacity(style.opacity.unwrap(), if frame == 0 { 1.0 } else { 0.0 });
        }
    }

    #[test]
    fn media_rules_use_the_live_width_and_height() {
        let css = r#"
            .base { color: black; }
            @media (max-width: 950px) { .narrow { color: green; } }
            @media (min-height: 900px) { .tall { color: blue; } }
            @media (orientation: portrait) { .portrait { color: red; } }
        "#;
        let selectors = |viewport| {
            parse_stylesheet_for_viewport(css, viewport)
                .into_iter()
                .map(|(selector, _)| selector)
                .collect::<Vec<_>>()
        };

        let narrow_tall = selectors((900.0, 1000.0));
        assert!(narrow_tall.iter().any(|s| s == ".base"));
        assert!(narrow_tall.iter().any(|s| s == ".narrow"));
        assert!(narrow_tall.iter().any(|s| s == ".tall"));
        assert!(narrow_tall.iter().any(|s| s == ".portrait"));

        let wide_short = selectors((1280.0, 720.0));
        assert!(wide_short.iter().any(|s| s == ".base"));
        assert!(!wide_short.iter().any(|s| s == ".narrow"));
        assert!(!wide_short.iter().any(|s| s == ".tall"));
        assert!(!wide_short.iter().any(|s| s == ".portrait"));
    }

    #[test]
    fn nested_media_rules_use_the_live_viewport() {
        let css = ".card { display:block; @media (max-width: 950px) { width:100%; } }";
        let narrow = parse_stylesheet_for_viewport(css, (900.0, 1000.0));
        let wide = parse_stylesheet_for_viewport(css, (1280.0, 720.0));
        assert!(narrow
            .iter()
            .any(|(selector, declarations)| selector == ".card"
                && declarations.contains("width:100%")));
        assert!(!wide
            .iter()
            .any(|(_, declarations)| declarations.contains("width:100%")));
    }

    #[test]
    fn tailwind_container_query_is_retained_but_simple_parser_omits_it() {
        let css = r#"
            .base { width: 10px }
            @container (min-width: 28rem) {
                .\@md\:flex-row { flex-direction: row }
            }
            @container main not (max-inline-size: 60em) {
                .named { width: 20px }
            }
        "#;
        let mut conditions = condition_arena_root();
        let parsed = parse_stylesheet_for_viewport_preserving_containers(
            css,
            (1280.0, 720.0),
            CssMediaType::Screen,
            &mut conditions,
            ContainerConditionId::NONE,
        );
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[1].container_condition_id, ContainerConditionId(1));
        assert_eq!(parsed[2].container_condition_id, ContainerConditionId(2));
        assert_eq!(conditions.len(), 3);
        assert_eq!(
            conditions[1].alternatives[0].condition,
            Some(ContainerQueryExpr::Feature(ContainerSizeFeature {
                axis: ContainerQueryAxis::Width,
                comparison: ContainerQueryComparison::Min,
                length: ContainerQueryLength::Rem(28.0),
            }))
        );
        assert_eq!(conditions[2].alternatives[0].name.as_deref(), Some("main"));
        assert!(matches!(
            conditions[2].alternatives[0].condition,
            Some(ContainerQueryExpr::Not(_))
        ));
        assert_eq!(parse_stylesheet(css).len(), 1);
    }

    #[test]
    fn container_at_rule_keyword_is_ascii_insensitive_and_exact() {
        assert_eq!(
            at_rule_prelude("CoNtAiNeR (min-width:1px)", "container"),
            Some("(min-width:1px)")
        );
        assert_eq!(
            at_rule_prelude("container/**/(min-width:1px)", "container"),
            Some("(min-width:1px)")
        );
        assert!(at_rule_prelude("containerfoo (min-width:1px)", "container").is_none());
        assert!(at_rule_prelude("container-type (min-width:1px)", "container").is_none());

        let css = r#"
            @CONTAINER (min-width:1px) {
                .top-level { width:1px }
            }
            .host {
                @CoNtAiNeR (min-width:2px) { width:2px }
                @containerfoo (min-width:3px) { height:3px }
            }
            .comment-host {
                @CONTAINER/**/(min-width:5px) { height:5px }
            }
            @containerfoo (min-width:4px) {
                .unknown-prefix { width:4px }
            }
        "#;
        let mut conditions = condition_arena_root();
        let parsed = parse_stylesheet_for_viewport_preserving_containers(
            css,
            (1280.0, 720.0),
            CssMediaType::Screen,
            &mut conditions,
            ContainerConditionId::NONE,
        );
        assert_eq!(conditions.len(), 4);
        assert_eq!(parsed.len(), 3, "unknown prefix at-rules must be dropped");
        assert!(parsed.iter().any(|rule| rule.selector == ".top-level"));
        assert!(parsed
            .iter()
            .any(|rule| { rule.selector == ".host" && rule.declarations.contains("width:2px") }));
        assert!(parsed.iter().any(|rule| {
            rule.selector == ".comment-host" && rule.declarations.contains("height:5px")
        }));
        assert!(!parsed.iter().any(|rule| {
            rule.selector == ".unknown-prefix" || rule.declarations.contains("height:3px")
        }));
    }

    #[test]
    fn global_at_rules_inside_container_are_not_conditioned() {
        let css = r#"
            @container (min-width:10000px) {
                @property --cq-token {
                    syntax: "<length>";
                    inherits: false;
                    initial-value: 17px;
                }
                .conditional { width:999px }
            }
        "#;
        let mut conditions = condition_arena_root();
        let parsed = parse_stylesheet_for_viewport_preserving_containers(
            css,
            (1280.0, 720.0),
            CssMediaType::Screen,
            &mut conditions,
            ContainerConditionId::NONE,
        );
        assert_eq!(conditions.len(), 2);
        let registration = parsed
            .iter()
            .find(|rule| {
                rule.selector == format!("{PROPERTY_REGISTRATION_SELECTOR_PREFIX}--cq-token")
            })
            .expect("@property initial value should be registered");
        assert_eq!(
            registration.container_condition_id,
            ContainerConditionId::NONE
        );
        assert!(registration.declarations.contains("initial-value: 17px"));
        let conditional = parsed
            .iter()
            .find(|rule| rule.selector == ".conditional")
            .expect("ordinary conditional rule should remain indexed");
        assert_eq!(conditional.container_condition_id, ContainerConditionId(1));
        assert!(parse_stylesheet(css).is_empty());
    }

    fn apply_registered_property_test_style(
        sheet: &Stylesheet,
        tree: &DomTree,
        target: NodeId,
        parent_props: &HashMap<String, String>,
    ) -> (LayoutStyle, HashMap<String, String>) {
        let node = tree.get_node(target).unwrap();
        let element = node.as_element().unwrap();
        let classes = node
            .get_attribute("class")
            .map(|value| {
                value
                    .split_whitespace()
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut matcher = tree.matcher();
        let mut style = LayoutStyle::default();
        let effective = sheet.apply(
            tree,
            &mut matcher,
            target,
            node.get_attribute("id"),
            &classes,
            element.local.as_ref(),
            &mut style,
            parent_props,
            None,
        );
        (style, effective.unwrap_or_else(|| parent_props.clone()))
    }

    #[test]
    fn registered_custom_properties_obey_inherits_descriptors() {
        let tree = obscura_dom::parse_html(
            r#"<div id="parent"><div id="child"></div></div><div id="initial"></div>"#,
        );
        let css = r#"
            @property --private {
                syntax:"<percentage>";
                inherits:false;
                initial-value:75%;
            }
            @property --shared {
                syntax:"<percentage>";
                inherits:true;
                initial-value:25%;
            }
            #parent { --private:20%; --shared:30% }
            #child, #initial {
                width:var(--private);
                height:var(--shared);
            }
        "#;
        let sheet = Stylesheet::parse(&tree, &[css.to_string()]);
        let parent = tree.get_element_by_id("parent").unwrap();
        let child = tree.get_element_by_id("child").unwrap();
        let initial = tree.get_element_by_id("initial").unwrap();
        let (_, parent_props) =
            apply_registered_property_test_style(&sheet, &tree, parent, &HashMap::new());
        let (child_style, _) =
            apply_registered_property_test_style(&sheet, &tree, child, &parent_props);
        assert_eq!(child_style.width, crate::Dimension::Percent(0.75));
        assert_eq!(child_style.height, crate::Dimension::Percent(0.30));

        let (initial_style, _) =
            apply_registered_property_test_style(&sheet, &tree, initial, &HashMap::new());
        assert_eq!(initial_style.width, crate::Dimension::Percent(0.75));
        assert_eq!(initial_style.height, crate::Dimension::Percent(0.25));
    }

    #[test]
    fn registered_custom_properties_reuse_an_unchanged_parent_map() {
        let tree = obscura_dom::parse_html(r#"<div id="target"></div>"#);
        let css = r#"
            @property --private {
                syntax:"<percentage>";
                inherits:false;
                initial-value:75%;
            }
            @property --shared {
                syntax:"<percentage>";
                inherits:true;
                initial-value:25%;
            }
            #target { width:var(--private); height:var(--shared) }
        "#;
        let sheet = Stylesheet::parse(&tree, &[css.to_string()]);
        let target = tree.get_element_by_id("target").unwrap();
        let parent_props = HashMap::from([
            ("--private".to_string(), "75%".to_string()),
            ("--shared".to_string(), "30%".to_string()),
        ]);
        let mut matcher = tree.matcher();
        let mut style = LayoutStyle::default();
        let effective = sheet.apply(
            &tree,
            &mut matcher,
            target,
            Some("target"),
            &[],
            "div",
            &mut style,
            &parent_props,
            None,
        );
        assert!(effective.is_none());
        assert_eq!(style.width, crate::Dimension::Percent(0.75));
        assert_eq!(style.height, crate::Dimension::Percent(0.30));

        let overridden_parent = HashMap::from([
            ("--private".to_string(), "60%".to_string()),
            ("--shared".to_string(), "30%".to_string()),
        ]);
        let mut style = LayoutStyle::default();
        let effective = sheet
            .apply(
                &tree,
                &mut matcher,
                target,
                Some("target"),
                &[],
                "div",
                &mut style,
                &overridden_parent,
                None,
            )
            .expect("non-inherited registered property must reset on the child");
        assert_eq!(effective.get("--private").map(String::as_str), Some("75%"));
        assert_eq!(effective.get("--shared").map(String::as_str), Some("30%"));
        assert_eq!(style.width, crate::Dimension::Percent(0.75));
        assert_eq!(style.height, crate::Dimension::Percent(0.30));
    }

    #[test]
    fn registered_custom_property_overrides_and_var_fallbacks_stay_distinct() {
        let tree = obscura_dom::parse_html(
            r#"<div id="override"></div><div id="reset"></div><div id="invalid"></div>"#,
        );
        let css = r#"
            @property --registered {
                syntax:"<percentage>";
                inherits:false;
                initial-value:75%;
            }
            #override {
                --registered:60%;
                width:var(--registered, 10%);
                height:var(--missing, 12%);
            }
            #reset {
                --registered:initial;
                width:var(--registered, 10%);
                height:var(--missing, 12%);
            }
            #invalid {
                --registered:red;
                width:var(--registered, 10%);
            }
        "#;
        let sheet = Stylesheet::parse(&tree, &[css.to_string()]);
        let override_id = tree.get_element_by_id("override").unwrap();
        let reset_id = tree.get_element_by_id("reset").unwrap();
        let invalid_id = tree.get_element_by_id("invalid").unwrap();
        let (overridden, _) =
            apply_registered_property_test_style(&sheet, &tree, override_id, &HashMap::new());
        assert_eq!(overridden.width, crate::Dimension::Percent(0.60));
        assert_eq!(overridden.height, crate::Dimension::Percent(0.12));
        let (reset, _) =
            apply_registered_property_test_style(&sheet, &tree, reset_id, &HashMap::new());
        assert_eq!(reset.width, crate::Dimension::Percent(0.75));
        assert_eq!(reset.height, crate::Dimension::Percent(0.12));
        let (invalid, invalid_props) =
            apply_registered_property_test_style(&sheet, &tree, invalid_id, &HashMap::new());
        assert_eq!(invalid.width, crate::Dimension::Percent(0.75));
        assert_eq!(
            invalid_props.get("--registered").map(String::as_str),
            Some("75%"),
            "an invalid typed value computes to the registered initial value"
        );
    }

    #[test]
    fn var_substitution_preserves_neighboring_token_boundaries() {
        let props = HashMap::from([
            ("--stroke".to_string(), "2px".to_string()),
            ("--amount".to_string(), "10".to_string()),
        ]);
        assert_eq!(
            substitute_var_value("var(--stroke)solid", &props, 0).as_deref(),
            Some("2px solid")
        );
        assert_eq!(
            substitute_var_value("calc(var(--stroke)*3)", &props, 0).as_deref(),
            Some("calc(2px*3)"),
            "punctuation adjacency must not gain calc-significant whitespace"
        );
        assert_eq!(
            substitute_var_value("calc(var(--amount)- 2px)", &props, 0).as_deref(),
            Some("calc(10- 2px)"),
            "substitution must not make an invalid unspaced calc minus valid"
        );
        assert_eq!(
            substitute_var_value("+var(--amount)", &props, 0).as_deref(),
            Some("+ 10"),
            "a delim plus followed by a number must not become a signed number token"
        );
        assert_eq!(
            substitute_var_value("var(--amount)%", &props, 0).as_deref(),
            Some("10 %"),
            "a number token followed by a percent delimiter is not a percentage token"
        );

        let tree = obscura_dom::parse_html(r#"<div id="target"></div>"#);
        let css = r#"
            #target {
                --stroke:2px;
                --ink:#123456;
                border:var(--stroke)solid var(--ink);
                height:calc(var(--stroke)*3);
            }
        "#;
        let sheet = Stylesheet::parse(&tree, &[css.to_string()]);
        let target = tree.get_element_by_id("target").unwrap();
        let (style, _) =
            apply_registered_property_test_style(&sheet, &tree, target, &HashMap::new());
        assert_eq!(
            style.border,
            crate::Edges {
                top: 2.0,
                right: 2.0,
                bottom: 2.0,
                left: 2.0,
            }
        );
        assert_eq!(style.border_color, Some([0x12, 0x34, 0x56, 0xff]));
        assert_eq!(style.height, crate::Dimension::Px(6.0));
    }

    #[test]
    fn wildcard_duplicate_and_invalid_property_registrations_are_bounded() {
        let tree = obscura_dom::parse_html(
            r#"<div id="wild"></div><div id="wild-fallback"></div><div id="typed"></div>"#,
        );
        let css = r#"
            @property --wild { syntax:"*"; inherits:false }
            @property --typed {
                syntax:"<number>";
                inherits:false;
                initial-value:2;
            }
            @property --typed {
                syntax:"<number>";
                initial-value:9;
            }
            @property --unsupported {
                syntax:"<angle>";
                inherits:false;
                initial-value:30deg;
            }
            @property --last {
                syntax:"<number>";
                inherits:false;
                initial-value:1;
            }
            @property --last {
                syntax:"<number>";
                inherits:false;
                initial-value:3;
            }
            #wild { --wild:20px; width:var(--wild, 7px) }
            #wild-fallback { width:var(--wild, 7px) }
            #typed {
                opacity:var(--typed, .1);
                height:var(--unsupported, 11px);
            }
        "#;
        let sheet = Stylesheet::parse(&tree, &[css.to_string()]);
        let wild = tree.get_element_by_id("wild").unwrap();
        let wild_fallback = tree.get_element_by_id("wild-fallback").unwrap();
        let typed = tree.get_element_by_id("typed").unwrap();
        let (wild_style, _) =
            apply_registered_property_test_style(&sheet, &tree, wild, &HashMap::new());
        assert_eq!(wild_style.width, crate::Dimension::Px(20.0));
        let (fallback_style, _) =
            apply_registered_property_test_style(&sheet, &tree, wild_fallback, &HashMap::new());
        assert_eq!(fallback_style.width, crate::Dimension::Px(7.0));
        let (typed_style, typed_props) =
            apply_registered_property_test_style(&sheet, &tree, typed, &HashMap::new());
        assert_eq!(typed_style.opacity, Some(2.0));
        assert_eq!(typed_style.height, crate::Dimension::Px(11.0));
        assert_eq!(
            typed_props.get("--typed").map(String::as_str),
            Some("2"),
            "a later invalid duplicate registration must not replace the valid one"
        );
        assert_eq!(
            typed_props.get("--last").map(String::as_str),
            Some("3"),
            "the last valid registration wins"
        );
        assert!(!typed_props.contains_key("--unsupported"));
    }

    #[test]
    fn registered_percentage_initial_value_keeps_radial_gradient_valid() {
        let tree = obscura_dom::parse_html(r#"<div id="pulse"></div>"#);
        let css = r#"
            @property --pulse-outer {
                syntax:"<percentage>";
                inherits:false;
                initial-value:75%;
            }
            #pulse {
                background-image:radial-gradient(
                    circle at 50% 50%,
                    transparent var(--pulse-outer),
                    rgb(255 255 255) 100%
                );
            }
        "#;
        let sheet = Stylesheet::parse(&tree, &[css.to_string()]);
        let pulse = tree.get_element_by_id("pulse").unwrap();
        let (style, props) =
            apply_registered_property_test_style(&sheet, &tree, pulse, &HashMap::new());
        assert_eq!(props.get("--pulse-outer").map(String::as_str), Some("75%"));
        let (_, stops) = style
            .background_radial_gradient
            .expect("registered initial value should preserve the gradient");
        assert_eq!(stops[0].1, Some(0.75));
        assert_eq!(stops[1].1, Some(1.0));
    }

    #[test]
    fn container_boolean_grammar_rejects_mixed_operators() {
        for invalid in [
            "(min-width:1px) and (max-width:2px) or (min-inline-size:3px)",
            "not (min-width:1px) and (max-width:2px)",
            "(min-width:1px) or not (max-width:2px)",
        ] {
            assert!(
                parse_container_query_expr(invalid).is_none(),
                "invalid boolean grammar was accepted: {invalid}"
            );
        }
        assert!(matches!(
            parse_container_query_expr(
                "(min-width:1px) and ((max-width:2px) or (min-inline-size:3px))"
            ),
            Some(ContainerQueryExpr::And(_))
        ));
        assert!(matches!(
            parse_container_query_expr("not ((min-width:1px) and (max-inline-size:2px))"),
            Some(ContainerQueryExpr::Not(_))
        ));
    }

    #[test]
    fn container_custom_ident_rejects_css_wide_and_default() {
        for reserved in [
            "none",
            "not",
            "and",
            "or",
            "default",
            "initial",
            "inherit",
            "unset",
            "revert",
            "revert-layer",
        ] {
            assert!(
                parse_container_query_name(reserved).is_none(),
                "reserved custom-ident was accepted: {reserved}"
            );
            if reserved != "not" {
                assert!(
                    parse_container_query(&format!("{reserved} (min-width:1px)")).is_none(),
                    "reserved query name was accepted: {reserved}"
                );
            }
        }
        assert!(
            matches!(
                parse_container_query("not (min-width:1px)"),
                Some(ContainerQuery {
                    name: None,
                    condition: Some(ContainerQueryExpr::Not(_)),
                })
            ),
            "`not` is reserved as a name but valid as the unary query operator"
        );
        for valid in ["auto", "normal", "container", "--card", "main"] {
            assert_eq!(
                parse_container_query_name(valid).as_deref(),
                Some(valid),
                "valid query name was rejected: {valid}"
            );
        }
    }

    #[test]
    fn unknown_comma_arm_does_not_drop_supported_arm() {
        let queries = parse_container_query_list("(future(foo)), main (min-width:1px)")
            .expect("general-enclosed arm is valid unknown syntax");
        assert_eq!(queries.len(), 2);
        assert_eq!(queries[0].condition, Some(ContainerQueryExpr::Unknown));
        assert_eq!(queries[1].name.as_deref(), Some("main"));
        assert!(matches!(
            queries[1].condition,
            Some(ContainerQueryExpr::Feature(_))
        ));

        let mut conditions = condition_arena_root();
        let parsed = parse_stylesheet_for_viewport_preserving_containers(
            "@container (future(foo)), main (min-width:1px) {.card{display:grid}}",
            (1280.0, 720.0),
            CssMediaType::Screen,
            &mut conditions,
            ContainerConditionId::NONE,
        );
        assert_eq!(parsed.len(), 1);
        assert_eq!(conditions[1].alternatives.len(), 2);
    }

    #[test]
    fn container_query_recursion_depth_is_bounded() {
        let nested = format!(
            "{}min-width:1px{}",
            "(".repeat(MAX_CONTAINER_QUERY_DEPTH + 16),
            ")".repeat(MAX_CONTAINER_QUERY_DEPTH + 16)
        );
        assert!(parse_container_query_expr(&nested).is_none());
    }

    #[test]
    fn supports_accepts_container_css_wide_values() {
        for property in ["container", "container-name", "container-type"] {
            for keyword in ["initial", "inherit", "unset", "revert", "revert-layer"] {
                assert!(
                    supports_condition_applies(&format!("({property}:{keyword})")),
                    "{property}:{keyword} is a valid whole-value CSS-wide declaration"
                );
            }
        }
    }

    fn evaluate_container_styles(
        tree: &DomTree,
        sheet: &Stylesheet,
        target: NodeId,
        snapshot: &ContainerSnapshot,
    ) -> (LayoutStyle, ContainerDecisionSignature, ContainerQueryStats) {
        let node = tree.get_node(target).unwrap();
        let element = node.as_element().unwrap();
        let id = node.get_attribute("id");
        let classes: Vec<String> = node
            .get_attribute("class")
            .map(|value| value.split_whitespace().map(str::to_string).collect())
            .unwrap_or_default();
        let mut matcher = tree.matcher();
        let mut evaluator = ContainerQueryEvaluator::new(tree, snapshot);
        let mut style = LayoutStyle::default();
        sheet.apply_with_container_queries(
            tree,
            &mut matcher,
            target,
            id,
            &classes,
            element.local.as_ref(),
            &mut style,
            &HashMap::new(),
            None,
            &mut evaluator,
        );
        let (signature, stats) = evaluator.finish();
        (style, signature, stats)
    }

    fn container_box(
        container_type: crate::ContainerType,
        names: &[&str],
        content_width: f32,
        font_size: f32,
    ) -> ContainerBox {
        ContainerBox {
            container_type,
            available_type: container_type,
            names: names.iter().map(|name| (*name).to_string()).collect(),
            content_width,
            content_height: 100.0,
            font_size,
        }
    }

    #[test]
    fn container_evaluator_honors_tailwind_threshold_and_cache() {
        let tree = obscura_dom::parse_html(r#"<div id="container"><div id="target"></div></div>"#);
        let container = tree.get_element_by_id("container").unwrap();
        let target = tree.get_element_by_id("target").unwrap();
        let sheet = Stylesheet::parse(
            &tree,
            &[r#"
                #target { width:1px; height:1px }
                @container (min-width:28rem) {
                    #target { width:2px }
                    #target { height:2px }
                }
            "#
            .to_string()],
        );

        let snapshot = |width| {
            let mut snapshot = ContainerSnapshot {
                root_font_size: 16.0,
                ..Default::default()
            };
            snapshot.boxes.insert(
                container,
                container_box(crate::ContainerType::InlineSize, &[], width, 16.0),
            );
            snapshot
        };
        let (below, _, _) = evaluate_container_styles(&tree, &sheet, target, &snapshot(447.0));
        assert_eq!(below.width, crate::Dimension::Px(1.0));
        assert_eq!(below.height, crate::Dimension::Px(1.0));

        let (at, _, stats) = evaluate_container_styles(&tree, &sheet, target, &snapshot(448.0));
        assert_eq!(at.width, crate::Dimension::Px(2.0));
        assert_eq!(at.height, crate::Dimension::Px(2.0));
        assert_eq!(stats.evaluations, 1);
        assert!(stats.cache_hits >= 1);
    }

    #[test]
    fn container_evaluator_matches_selector_before_ancestor_lookup() {
        let tree = obscura_dom::parse_html(r#"<div id="container"><div id="target"></div></div>"#);
        let container = tree.get_element_by_id("container").unwrap();
        let target = tree.get_element_by_id("target").unwrap();
        let sheet = Stylesheet::parse(
            &tree,
            &[r#"
                @container (min-width:1px) {
                    div[data-never-present] { width:999px }
                }
            "#
            .to_string()],
        );
        let mut snapshot = ContainerSnapshot {
            root_font_size: 16.0,
            ..Default::default()
        };
        snapshot.boxes.insert(
            container,
            container_box(crate::ContainerType::InlineSize, &[], 500.0, 16.0),
        );
        let (_, signature, stats) = evaluate_container_styles(&tree, &sheet, target, &snapshot);
        assert_eq!(stats.evaluations, 0);
        assert_eq!(stats.ancestor_steps, 0);
        assert!(signature.decisions.is_empty());
    }

    #[test]
    fn container_evaluator_selects_nearest_eligible_named_ancestor() {
        let tree = obscura_dom::parse_html(
            r#"<div id="outer"><div id="inner"><div id="target"></div></div></div>"#,
        );
        let outer = tree.get_element_by_id("outer").unwrap();
        let inner = tree.get_element_by_id("inner").unwrap();
        let target = tree.get_element_by_id("target").unwrap();
        let sheet = Stylesheet::parse(
            &tree,
            &[r#"
                #target { width:1px; height:1px }
                @container shell (min-width:500px) { #target { width:11px } }
                @container (min-width:500px) { #target { height:22px } }
            "#
            .to_string()],
        );
        let mut snapshot = ContainerSnapshot {
            root_font_size: 16.0,
            ..Default::default()
        };
        snapshot.boxes.insert(
            outer,
            container_box(crate::ContainerType::InlineSize, &["shell"], 600.0, 16.0),
        );
        snapshot.boxes.insert(
            inner,
            container_box(crate::ContainerType::InlineSize, &["other"], 300.0, 16.0),
        );
        let (style, _, stats) = evaluate_container_styles(&tree, &sheet, target, &snapshot);
        assert_eq!(style.width, crate::Dimension::Px(11.0));
        assert_eq!(style.height, crate::Dimension::Px(1.0));
        assert!(stats.ancestor_steps >= 3);
    }

    #[test]
    fn container_query_em_uses_container_font_and_rem_uses_root_font() {
        let tree = obscura_dom::parse_html(r#"<div id="container"><div id="target"></div></div>"#);
        let container = tree.get_element_by_id("container").unwrap();
        let target = tree.get_element_by_id("target").unwrap();
        let sheet = Stylesheet::parse(
            &tree,
            &[r#"
                #target { width:1px; height:1px }
                @container (min-width:30em) { #target { width:3px } }
                @container (min-width:30rem) { #target { height:3px } }
            "#
            .to_string()],
        );
        let mut snapshot = ContainerSnapshot {
            root_font_size: 20.0,
            ..Default::default()
        };
        snapshot.boxes.insert(
            container,
            container_box(crate::ContainerType::InlineSize, &[], 400.0, 10.0),
        );
        let (style, _, _) = evaluate_container_styles(&tree, &sheet, target, &snapshot);
        assert_eq!(style.width, crate::Dimension::Px(3.0));
        assert_eq!(style.height, crate::Dimension::Px(1.0));
    }

    #[test]
    fn container_snapshot_compares_only_axes_exposed_by_container_type() {
        let mut inline_a = container_box(crate::ContainerType::InlineSize, &[], 400.0, 16.0);
        let mut inline_b = inline_a.clone();
        inline_a.content_height = 100.0;
        inline_b.content_height = 900.0;
        assert_eq!(inline_a, inline_b);

        let mut size_a = container_box(crate::ContainerType::Size, &[], 400.0, 16.0);
        let mut size_b = size_a.clone();
        size_a.content_height = 100.0;
        size_b.content_height = 900.0;
        assert_ne!(size_a, size_b);
    }

    #[test]
    fn nested_container_conditions_select_independent_containers() {
        let tree = obscura_dom::parse_html(
            r#"<div id="outer"><div id="inner"><div id="target"></div></div></div>"#,
        );
        let outer = tree.get_element_by_id("outer").unwrap();
        let inner = tree.get_element_by_id("inner").unwrap();
        let target = tree.get_element_by_id("target").unwrap();
        let sheet = Stylesheet::parse(
            &tree,
            &[r#"
                #target { width:1px }
                @container outer (min-width:500px) {
                    @container inner (min-width:200px) {
                        #target { width:9px }
                    }
                }
            "#
            .to_string()],
        );
        let snapshot = |inner_width| {
            let mut snapshot = ContainerSnapshot {
                root_font_size: 16.0,
                ..Default::default()
            };
            snapshot.boxes.insert(
                outer,
                container_box(crate::ContainerType::InlineSize, &["outer"], 600.0, 16.0),
            );
            snapshot.boxes.insert(
                inner,
                container_box(
                    crate::ContainerType::InlineSize,
                    &["inner"],
                    inner_width,
                    16.0,
                ),
            );
            snapshot
        };
        let (matching, _, _) = evaluate_container_styles(&tree, &sheet, target, &snapshot(200.0));
        assert_eq!(matching.width, crate::Dimension::Px(9.0));
        let (failing, _, _) = evaluate_container_styles(&tree, &sheet, target, &snapshot(199.0));
        assert_eq!(failing.width, crate::Dimension::Px(1.0));
    }

    #[test]
    fn unknown_container_alternative_does_not_mask_true_alternative() {
        let tree = obscura_dom::parse_html(r#"<div id="container"><div id="target"></div></div>"#);
        let container = tree.get_element_by_id("container").unwrap();
        let target = tree.get_element_by_id("target").unwrap();
        let sheet = Stylesheet::parse(
            &tree,
            &[r#"
                #target { width:1px }
                @container (future(foo)), (min-width:100px) {
                    #target { width:7px }
                }
            "#
            .to_string()],
        );
        let mut snapshot = ContainerSnapshot {
            root_font_size: 16.0,
            ..Default::default()
        };
        snapshot.boxes.insert(
            container,
            container_box(crate::ContainerType::InlineSize, &[], 100.0, 16.0),
        );
        let (style, _, _) = evaluate_container_styles(&tree, &sheet, target, &snapshot);
        assert_eq!(style.width, crate::Dimension::Px(7.0));
    }

    #[test]
    fn container_query_boolean_evaluation_uses_kleene_truth_tables() {
        let container = container_box(crate::ContainerType::InlineSize, &[], 200.0, 16.0);
        let min_width = |threshold| {
            ContainerQueryExpr::Feature(ContainerSizeFeature {
                axis: ContainerQueryAxis::Width,
                comparison: ContainerQueryComparison::Min,
                length: ContainerQueryLength::Px(threshold),
            })
        };
        assert_eq!(
            evaluate_container_query_expr(
                &ContainerQueryExpr::Or(vec![min_width(100.0), ContainerQueryExpr::Unknown,]),
                &container,
                16.0,
            ),
            ContainerQueryTruth::True
        );
        assert_eq!(
            evaluate_container_query_expr(
                &ContainerQueryExpr::And(vec![min_width(300.0), ContainerQueryExpr::Unknown,]),
                &container,
                16.0,
            ),
            ContainerQueryTruth::False
        );
        assert_eq!(
            evaluate_container_query_expr(
                &ContainerQueryExpr::Or(vec![min_width(300.0), ContainerQueryExpr::Unknown,]),
                &container,
                16.0,
            ),
            ContainerQueryTruth::Unknown
        );
        assert_eq!(
            evaluate_container_query_expr(
                &ContainerQueryExpr::Not(Box::new(ContainerQueryExpr::Unknown)),
                &container,
                16.0,
            ),
            ContainerQueryTruth::Unknown
        );
    }

    #[test]
    fn container_range_syntax_supports_strict_inclusive_and_chained_queries() {
        let container = container_box(crate::ContainerType::Size, &[], 200.0, 16.0);
        for query in [
            "(width)",
            "(width > 199px)",
            "(width>=200px)",
            "(199px < width)",
            "(199px < width <= 200px)",
            "(height = 100px)",
            "(block-size >= 100px)",
        ] {
            let expression = parse_container_query_expr(query).expect("valid range query");
            assert_eq!(
                evaluate_container_query_expr(&expression, &container, 16.0),
                ContainerQueryTruth::True,
                "{query}"
            );
        }
        for query in [
            "(width > 200px)",
            "(width < 200px)",
            "(200px < width < 300px)",
            "(height > 100px)",
        ] {
            let expression = parse_container_query_expr(query).expect("valid range query");
            assert_eq!(
                evaluate_container_query_expr(&expression, &container, 16.0),
                ContainerQueryTruth::False,
                "{query}"
            );
        }
        for invalid in [
            "(100px < width > 200px)",
            "(200px > width < 100px)",
            "(100px = width = 100px)",
            "(100px < width = 200px)",
        ] {
            assert!(
                parse_container_query_expr(invalid).is_none(),
                "invalid mixed/equality chain parsed: {invalid}"
            );
        }
    }

    #[test]
    fn block_axis_query_requires_size_container() {
        let tree = obscura_dom::parse_html(r#"<div id="container"><div id="target"></div></div>"#);
        let container = tree.get_element_by_id("container").unwrap();
        let target = tree.get_element_by_id("target").unwrap();
        let sheet = Stylesheet::parse(
            &tree,
            &[r#"
                #target { width:1px }
                @container (height >= 100px) { #target { width:8px } }
            "#
            .to_string()],
        );
        let snapshot = |container_type| {
            let mut snapshot = ContainerSnapshot {
                root_font_size: 16.0,
                ..Default::default()
            };
            snapshot
                .boxes
                .insert(container, container_box(container_type, &[], 300.0, 16.0));
            snapshot
        };
        let (inline_only, _, _) = evaluate_container_styles(
            &tree,
            &sheet,
            target,
            &snapshot(crate::ContainerType::InlineSize),
        );
        assert_eq!(inline_only.width, crate::Dimension::Px(1.0));
        let (size, _, _) =
            evaluate_container_styles(&tree, &sheet, target, &snapshot(crate::ContainerType::Size));
        assert_eq!(size.width, crate::Dimension::Px(8.0));
    }

    #[test]
    fn nested_container_rules_form_a_parent_condition_chain() {
        let css = "@container shell (min-width:40rem){\
            @container (max-inline-size:50rem){.card{display:grid}}}";
        let mut conditions = condition_arena_root();
        let parsed = parse_stylesheet_for_viewport_preserving_containers(
            css,
            (1280.0, 720.0),
            CssMediaType::Screen,
            &mut conditions,
            ContainerConditionId::NONE,
        );
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].container_condition_id, ContainerConditionId(2));
        assert_eq!(conditions[2].parent, ContainerConditionId(1));
        assert_eq!(conditions[1].parent, ContainerConditionId::NONE);
    }

    #[test]
    fn unresolved_container_rules_do_not_enter_cascade_or_pseudos() {
        let tree = obscura_dom::parse_html(r#"<div id="target"></div>"#);
        let target = tree.query_selector("#target").unwrap().unwrap();
        let sheet = Stylesheet::parse(
            &tree,
            &[r#"
            #target{width:10px}
            @container (min-width:28rem){
                #target{width:999px}
                #target::before{content:"inactive"}
            }
            #target{height:20px}
        "#
            .to_string()],
        );
        assert_eq!(sheet.rules.len(), 3, "conditional rule remains indexed");
        assert_eq!(sheet.container_conditions.len(), 2);
        let mut matcher = tree.matcher();
        let mut style = LayoutStyle::default();
        sheet.apply(
            &tree,
            &mut matcher,
            target,
            Some("target"),
            &[],
            "div",
            &mut style,
            &HashMap::new(),
            None,
        );
        assert_eq!(style.width, crate::Dimension::Px(10.0));
        assert_eq!(style.height, crate::Dimension::Px(20.0));
        let (before, after) =
            sheet.pseudo_styles(&tree, &mut matcher, target, &HashMap::new(), &style);
        assert!(before.is_none() && after.is_none());
    }

    #[test]
    fn no_container_parser_output_and_order_are_unchanged() {
        let css = ".card{width:10px}\
            @supports (display:grid){.card{display:grid}}\
            @media (min-width:64rem){.card{width:20px}}\
            .card{height:30px}";
        assert_eq!(
            parse_stylesheet_for_viewport(css, (1280.0, 720.0)),
            vec![
                (".card".into(), "width:10px;".into()),
                (".card".into(), "display:grid;".into()),
                (".card".into(), "width:20px;".into()),
                (".card".into(), "height:30px;".into()),
            ]
        );
        let mut conditions = condition_arena_root();
        let rich = parse_stylesheet_for_viewport_preserving_containers(
            css,
            (1280.0, 720.0),
            CssMediaType::Screen,
            &mut conditions,
            ContainerConditionId::NONE,
        );
        assert_eq!(conditions.len(), 1);
        assert!(rich
            .iter()
            .all(|rule| rule.container_condition_id == ContainerConditionId::NONE));
    }

    #[test]
    fn supports_conditions_gate_legacy_framework_fallbacks() {
        let legacy_probe = "(((-webkit-hyphens:none)) and \
            (not (margin-trim:inline))) or \
            ((-moz-orient:inline) and \
            (not (color:rgb(from red r g b))))";
        assert!(!supports_condition_applies(legacy_probe));
        assert!(supports_condition_applies(
            "(display:grid) and (selector(.card > *))"
        ));
        assert!(supports_condition_applies(
            "not (unknown-engine-prop:value)"
        ));

        let css = format!(
            "@supports {legacy_probe} {{ .legacy {{ line-height:1.5 }} }}\
             @supports (display:grid) {{ .modern {{ display:grid }} }}\
             .host {{ @supports {legacy_probe} {{ width:999px }} }}"
        );
        let rules = parse_stylesheet_for_viewport(&css, (1280.0, 720.0));
        assert!(
            !rules.iter().any(|(selector, declarations)| {
                selector == ".legacy" || declarations.contains("999px")
            }),
            "false supports branches must be skipped: {rules:?}"
        );
        assert!(
            rules.iter().any(|(selector, declarations)| {
                selector == ".modern" && declarations.contains("display:grid")
            }),
            "true supports branch should remain: {rules:?}"
        );
    }

    #[test]
    fn supports_conditions_reject_invalid_boolean_grammar() {
        assert!(supports_condition_applies(
            "(display:grid) and (word-break:break-all)"
        ));
        assert!(supports_condition_applies(
            "(display:grid) or (unknown:value)"
        ));
        assert!(!supports_condition_applies(
            "(display:grid) and (word-break:break-all) or (display:flex)"
        ));
        assert!(!supports_condition_applies(
            "not ((display:grid) and (word-break:break-all) or (display:flex))"
        ));
        assert!(!supports_condition_applies("not ()"));
        assert!(!supports_condition_applies("(display:grid;)"));
        assert!(!supports_condition_applies("not (display:grid"));
    }

    #[test]
    fn supports_polygon_clip_path_activates_only_painted_shape_subset() {
        let css = "@supports (clip-path:polygon(0 0,100% 0,50% 100%)){\
                       .transition{height:90px}\
                   }\
                   @supports (clip-path:polygon(0 0,100% 0,50% 100%) content-box){\
                       .unsupported{height:999px}\
                   }";
        let rules = parse_stylesheet_for_viewport(css, (1280.0, 720.0));
        assert!(
            rules.iter().any(|(selector, declarations)| {
                selector == ".transition" && declarations.contains("height:90px")
            }),
            "the supported polygon branch must enter the cascade: {rules:?}"
        );
        assert!(
            !rules.iter().any(|(selector, _)| selector == ".unsupported"),
            "an unpainted geometry-box variant must not pass @supports: {rules:?}"
        );
    }

    #[test]
    fn media_breakpoints_support_font_relative_lengths_and_ranges() {
        assert!(!media_query_applies_for_viewport(
            "@media (min-width: 64rem)",
            (900.0, 1000.0)
        ));
        assert!(media_query_applies_for_viewport(
            "@media (min-width: 64rem)",
            (1024.0, 768.0)
        ));
        assert!(media_query_applies_for_viewport(
            "@media (56.25rem <= width)",
            (900.0, 1000.0)
        ));
        assert!(!media_query_applies_for_viewport(
            "@media (width > calc(60em - 1px))",
            (900.0, 1000.0)
        ));
        let two_sidebar_breakpoint =
            "@media (width < calc(1rem * 2 + (15rem + 2rem) * 2 + 31rem))";
        assert!(media_query_applies_for_viewport(
            two_sidebar_breakpoint,
            (1000.0, 900.0)
        ));
        assert!(!media_query_applies_for_viewport(
            two_sidebar_breakpoint,
            (1440.0, 1000.0)
        ));
        let left_width_calc =
            "@media (calc(1rem * 2 + (15rem + 2rem) * 2 + 31rem) <= width)";
        assert!(!media_query_applies_for_viewport(
            left_width_calc,
            (1000.0, 900.0)
        ));
        assert!(media_query_applies_for_viewport(
            left_width_calc,
            (1440.0, 1000.0)
        ));
        let left_height_calc = "@media (calc(40rem + (2rem * 2)) < height)";
        assert!(!media_query_applies_for_viewport(
            left_height_calc,
            (1280.0, 704.0)
        ));
        assert!(media_query_applies_for_viewport(
            left_height_calc,
            (1280.0, 705.0)
        ));
    }

    #[test]
    fn media_type_selects_screen_print_negation_and_query_lists() {
        let viewport = (800.0, 600.0);
        let applies = |query, media| {
            media_query_applies_for_viewport_and_type(query, viewport, media)
        };

        assert!(applies("screen", CssMediaType::Screen));
        assert!(!applies("screen", CssMediaType::Print));
        assert!(applies("print", CssMediaType::Print));
        assert!(!applies("print", CssMediaType::Screen));
        assert!(applies("not print", CssMediaType::Screen));
        assert!(!applies("not print", CssMediaType::Print));
        assert!(applies("not screen", CssMediaType::Print));
        assert!(!applies("not screen", CssMediaType::Screen));
        assert!(applies("speech, print", CssMediaType::Print));
        assert!(!applies("speech, print", CssMediaType::Screen));
        assert!(applies(
            "print and (min-width: 700px)",
            CssMediaType::Print
        ));
        assert!(!applies(
            "print and (min-width: 900px)",
            CssMediaType::Print
        ));
        assert!(applies(
            "not all and (min-width: 900px)",
            CssMediaType::Screen
        ));
    }

    #[test]
    fn negated_min_width_queries_form_max_breakpoints() {
        assert!(media_query_applies_for_viewport(
            "@media not all and (min-width: 40rem)",
            (639.0, 900.0)
        ));
        assert!(!media_query_applies_for_viewport(
            "@media not all and (min-width: 40rem)",
            (1280.0, 900.0)
        ));

        let css = r#"
            .hidden { display: none }
            @media not all and (min-width: 40rem) {
                .max-sm\:inline { display: inline }
            }
            @media (min-width: 80rem) {
                .xl\:inline { display: inline }
            }
        "#;
        let desktop = parse_stylesheet_for_viewport(css, (1280.0, 900.0));
        assert!(!desktop
            .iter()
            .any(|(selector, _)| selector == r".max-sm\:inline"));
        assert!(desktop
            .iter()
            .any(|(selector, _)| selector == r".xl\:inline"));
    }

    #[test]
    fn media_query_lists_are_or_conditions() {
        assert!(media_query_applies_for_viewport(
            "@media print, (min-width: 64rem)",
            (1280.0, 720.0)
        ));
        assert!(!media_query_applies_for_viewport(
            "@media print, (min-width: 64rem)",
            (900.0, 1000.0)
        ));
    }

    #[test]
    fn rem_breakpoint_does_not_reveal_desktop_menu_on_narrow_viewport() {
        let css = r#"
            header .menu-toolkit { display: none }
            @media (min-width: 64rem) {
                header .menu-toolkit { display: flex }
            }
        "#;
        let narrow = parse_stylesheet_for_viewport(css, (900.0, 1000.0));
        assert!(narrow.iter().any(|(selector, declarations)| {
            selector == "header .menu-toolkit" && declarations.contains("display: none")
        }));
        assert!(!narrow
            .iter()
            .any(|(_, declarations)| declarations.contains("display: flex")));

        let wide = parse_stylesheet_for_viewport(css, (1024.0, 768.0));
        assert!(wide
            .iter()
            .any(|(_, declarations)| declarations.contains("display: flex")));
    }
}
