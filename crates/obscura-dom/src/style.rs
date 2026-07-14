//! Lightweight author-style cascade for Obscura's non-rendering DOM.
//!
//! This deliberately stops before layout and paint. It parses real-world CSS,
//! matches selectors, applies the author cascade, resolves custom properties,
//! and exposes enough retained rule metadata for CSSOM. Unknown declarations
//! and unsupported selectors are isolated to that declaration/selector rather
//! than invalidating the rest of a production stylesheet.

use std::collections::{HashMap, HashSet};

use cssparser::{
    AtRuleParser, BasicParseErrorKind, CowRcStr, DeclarationParser, ParseError, Parser,
    ParserInput, ParserState, QualifiedRuleParser, RuleBodyItemParser, RuleBodyParser,
    StyleSheetParser, Token,
};
use selectors::context::QuirksMode;
use selectors::matching::{
    MatchingContext, MatchingForInvalidation, MatchingMode, NeedsSelectorFlags,
};
use selectors::SelectorList;

use crate::selector::{parse_selector, DomElement, ObscuraSelector};
use crate::{DomTree, NodeId};

pub const MAX_CSS_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_RULES: usize = 50_000;
pub const MAX_SHEETS: usize = 256;
pub const MAX_IMPORT_DEPTH: usize = 8;
const MAX_COMPUTED_CACHE: usize = 1_024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaEnvironment {
    pub width: u32,
    pub height: u32,
    pub dark: bool,
    pub reduced_motion: bool,
    pub hover: bool,
    pub pointer_fine: bool,
}

impl Default for MediaEnvironment {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1000,
            dark: true,
            reduced_motion: false,
            hover: true,
            pointer_fine: true,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CssRuleInfo {
    pub css_text: String,
    pub rule_type: u16,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StyleSheetInfo {
    pub id: u32,
    pub owner_node: Option<u32>,
    pub href: Option<String>,
    pub media: String,
    pub disabled: bool,
    pub same_origin: bool,
    pub rule_count: usize,
}

#[derive(Debug, Clone)]
struct Declaration {
    name: String,
    value: String,
    important: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum RuleKey {
    Id(String),
    Class(String),
    Tag(String),
    Universal,
}

struct CompiledRule {
    selector: SelectorList<ObscuraSelector>,
    key: RuleKey,
    specificity: u32,
    declarations: Vec<Declaration>,
    media: Vec<String>,
    sheet_id: u32,
    source_order: u64,
}

struct StyleSheet {
    info: StyleSheetInfo,
    rules: Vec<CssRuleInfo>,
    active: bool,
    decoded_bytes: usize,
}

#[derive(Debug, Clone)]
struct Winner {
    value: String,
    important: bool,
    specificity: u32,
    order: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    node: NodeId,
    environment: MediaEnvironment,
}

/// Per-document stylesheet registry and lazy cascade cache.
pub struct StyleEngine {
    sheets: Vec<StyleSheet>,
    rules: Vec<CompiledRule>,
    by_id: HashMap<String, Vec<usize>>,
    by_class: HashMap<String, Vec<usize>>,
    by_tag: HashMap<String, Vec<usize>>,
    universal: Vec<usize>,
    next_sheet_id: u32,
    next_order: u64,
    decoded_bytes: usize,
    truncated: bool,
    epoch: u64,
    cache: HashMap<CacheKey, (u64, HashMap<String, String>)>,
    imports_by_owner: HashMap<NodeId, Vec<u32>>,
}

impl Default for StyleEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl StyleEngine {
    pub fn new() -> Self {
        Self {
            sheets: Vec::new(),
            rules: Vec::new(),
            by_id: HashMap::new(),
            by_class: HashMap::new(),
            by_tag: HashMap::new(),
            universal: Vec::new(),
            next_sheet_id: 1,
            next_order: 0,
            decoded_bytes: 0,
            truncated: false,
            epoch: 0,
            cache: HashMap::new(),
            imports_by_owner: HashMap::new(),
        }
    }

    pub fn clear(&mut self) {
        *self = Self::new();
    }

    pub fn register_stylesheet(
        &mut self,
        owner_node: Option<NodeId>,
        href: Option<String>,
        media: String,
        same_origin: bool,
        css: &str,
    ) -> Option<u32> {
        if self.sheets.len() >= MAX_SHEETS {
            tracing::warn!(
                limit = MAX_SHEETS,
                "CSS stylesheet limit reached; ignoring sheet"
            );
            self.truncated = true;
            return None;
        }
        let remaining = MAX_CSS_BYTES.saturating_sub(self.decoded_bytes);
        let css = if css.len() > remaining {
            tracing::warn!(
                limit = MAX_CSS_BYTES,
                "CSS byte limit reached; stylesheet truncated"
            );
            self.truncated = true;
            let mut end = remaining;
            while end > 0 && !css.is_char_boundary(end) {
                end -= 1;
            }
            &css[..end]
        } else {
            css
        };

        let id = self.next_sheet_id;
        self.next_sheet_id += 1;
        self.decoded_bytes += css.len();

        let mut parser = SheetParser::new(id, self.next_order, self.rules.len());
        let conditions = if media.trim().is_empty() {
            Vec::new()
        } else {
            vec![media.clone()]
        };
        parser.parse(css, conditions);
        self.next_order = parser.next_order;
        if parser.hit_rule_limit {
            tracing::warn!(
                limit = MAX_RULES,
                "CSS rule limit reached; stylesheet truncated"
            );
            self.truncated = true;
        }

        for rule in parser.compiled {
            if self.rules.len() >= MAX_RULES {
                break;
            }
            let index = self.rules.len();
            match &rule.key {
                RuleKey::Id(value) => self.by_id.entry(value.clone()).or_default().push(index),
                RuleKey::Class(value) => {
                    self.by_class.entry(value.clone()).or_default().push(index)
                }
                RuleKey::Tag(value) => self.by_tag.entry(value.clone()).or_default().push(index),
                RuleKey::Universal => self.universal.push(index),
            }
            self.rules.push(rule);
        }

        let rule_count = parser.css_rules.len();
        self.sheets.push(StyleSheet {
            info: StyleSheetInfo {
                id,
                owner_node: owner_node.map(|node| node.raw()),
                href,
                media,
                disabled: false,
                same_origin,
                rule_count,
            },
            rules: parser.css_rules,
            active: true,
            decoded_bytes: css.len(),
        });
        self.invalidate();
        Some(id)
    }

    pub fn register_constructed(&mut self, css: &str) -> Option<u32> {
        let id = self.register_stylesheet(None, None, String::new(), true, css)?;
        if let Some(sheet) = self.sheets.iter_mut().find(|sheet| sheet.info.id == id) {
            sheet.active = false;
        }
        self.invalidate();
        Some(id)
    }

    pub fn register_import_stylesheet(
        &mut self,
        owner_node: NodeId,
        href: String,
        media: String,
        same_origin: bool,
        css: &str,
    ) -> Option<u32> {
        let id = self.register_stylesheet(None, Some(href), media, same_origin, css)?;
        self.imports_by_owner.entry(owner_node).or_default().push(id);
        Some(id)
    }

    pub fn set_adopted(&mut self, ids: &[u32]) {
        let adopted: HashSet<u32> = ids.iter().copied().collect();
        for sheet in &mut self.sheets {
            if sheet.info.owner_node.is_none() && sheet.info.href.is_none() {
                sheet.active = adopted.contains(&sheet.info.id);
            }
        }
        self.invalidate();
    }

    pub fn replace_owner_stylesheet(
        &mut self,
        owner_node: NodeId,
        href: Option<String>,
        media: String,
        same_origin: bool,
        css: &str,
    ) -> Option<u32> {
        self.remove_owner(owner_node);
        self.register_stylesheet(Some(owner_node), href, media, same_origin, css)
    }

    pub fn remove_owner(&mut self, owner_node: NodeId) {
        let before = self.sheets.len();
        let imported = self.imports_by_owner.remove(&owner_node).unwrap_or_default();
        self.sheets
            .retain(|sheet| {
                sheet.info.owner_node != Some(owner_node.raw())
                    && !imported.contains(&sheet.info.id)
            });
        if before != self.sheets.len() {
            self.decoded_bytes = self.sheets.iter().map(|sheet| sheet.decoded_bytes).sum();
            self.rebuild_rule_indexes();
        }
    }

    pub fn set_disabled(&mut self, sheet_id: u32, disabled: bool) -> bool {
        if let Some(sheet) = self
            .sheets
            .iter_mut()
            .find(|sheet| sheet.info.id == sheet_id)
        {
            if sheet.info.disabled != disabled {
                sheet.info.disabled = disabled;
                self.invalidate();
            }
            true
        } else {
            false
        }
    }

    pub fn sheet_infos(&self) -> Vec<StyleSheetInfo> {
        self.sheets.iter().map(|sheet| sheet.info.clone()).collect()
    }

    pub fn sheet_rules(&self, sheet_id: u32) -> Option<Vec<CssRuleInfo>> {
        self.sheets
            .iter()
            .find(|sheet| sheet.info.id == sheet_id)
            .map(|sheet| sheet.rules.clone())
    }

    pub fn insert_rule(
        &mut self,
        sheet_id: u32,
        css: &str,
        index: usize,
    ) -> Result<(usize, u32), String> {
        let other_rule_count = self
            .rules
            .iter()
            .filter(|rule| rule.sheet_id != sheet_id)
            .count();
        let sheet = self
            .sheets
            .iter_mut()
            .find(|sheet| sheet.info.id == sheet_id)
            .ok_or_else(|| "stylesheet not found".to_string())?;
        if index > sheet.rules.len() {
            return Err("IndexSizeError".to_string());
        }
        let mut parser = SheetParser::new(sheet_id, self.next_order, other_rule_count);
        parser.parse(css, Vec::new());
        let Some(info) = parser.css_rules.into_iter().next() else {
            return Err("SyntaxError".to_string());
        };
        sheet.rules.insert(index, info);
        sheet.info.rule_count = sheet.rules.len();
        let combined = sheet
            .rules
            .iter()
            .map(|rule| rule.css_text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let owner = sheet.info.owner_node.map(NodeId::new);
        let href = sheet.info.href.clone();
        let media = sheet.info.media.clone();
        let same_origin = sheet.info.same_origin;
        let constructed = owner.is_none() && href.is_none();
        let was_active = sheet.active;
        self.remove_sheet(sheet_id);
        let new_id = (if constructed {
            self.register_constructed(&combined)
        } else {
            self.register_stylesheet(owner, href, media, same_origin, &combined)
        })
        .ok_or_else(|| "stylesheet limit reached".to_string())?;
        if constructed && was_active {
            self.set_sheet_active(new_id, true);
        }
        Ok((index, new_id))
    }

    pub fn delete_rule(&mut self, sheet_id: u32, index: usize) -> Result<u32, String> {
        let sheet = self
            .sheets
            .iter_mut()
            .find(|sheet| sheet.info.id == sheet_id)
            .ok_or_else(|| "stylesheet not found".to_string())?;
        if index >= sheet.rules.len() {
            return Err("IndexSizeError".to_string());
        }
        sheet.rules.remove(index);
        sheet.info.rule_count = sheet.rules.len();
        // CSSOM mutations are rare. Reparse the retained text to keep the
        // matching representation and source order exactly aligned.
        let css = sheet
            .rules
            .iter()
            .map(|rule| rule.css_text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let owner = sheet.info.owner_node.map(NodeId::new);
        let href = sheet.info.href.clone();
        let media = sheet.info.media.clone();
        let same_origin = sheet.info.same_origin;
        let constructed = owner.is_none() && href.is_none();
        let was_active = sheet.active;
        self.remove_sheet(sheet_id);
        let new_id = (if constructed {
            self.register_constructed(&css)
        } else {
            self.register_stylesheet(owner, href, media, same_origin, &css)
        })
        .ok_or_else(|| "stylesheet limit reached".to_string())?;
        if constructed && was_active {
            self.set_sheet_active(new_id, true);
        }
        Ok(new_id)
    }

    pub fn replace_sheet(&mut self, sheet_id: u32, css: &str) -> Result<u32, String> {
        let (info, was_active) = self
            .sheets
            .iter()
            .find(|sheet| sheet.info.id == sheet_id)
            .map(|sheet| (sheet.info.clone(), sheet.active))
            .ok_or_else(|| "stylesheet not found".to_string())?;
        self.remove_sheet(sheet_id);
        let constructed = info.owner_node.is_none() && info.href.is_none();
        let new_id = (if constructed {
            self.register_constructed(css)
        } else {
            self.register_stylesheet(
                info.owner_node.map(NodeId::new),
                info.href,
                info.media,
                info.same_origin,
                css,
            )
        })
        .ok_or_else(|| "stylesheet limit reached".to_string())?;
        if constructed && was_active {
            self.set_sheet_active(new_id, true);
        }
        Ok(new_id)
    }

    pub fn compute_style(
        &mut self,
        dom: &DomTree,
        node: NodeId,
        environment: MediaEnvironment,
    ) -> HashMap<String, String> {
        let key = CacheKey { node, environment };
        if let Some((epoch, cached)) = self.cache.get(&key) {
            if *epoch == self.epoch {
                return cached.clone();
            }
        }
        let computed = self.compute_uncached(dom, node, environment, 0);
        if self.cache.len() >= MAX_COMPUTED_CACHE {
            self.cache.clear();
        }
        self.cache.insert(key, (self.epoch, computed.clone()));
        computed
    }

    fn compute_uncached(
        &self,
        dom: &DomTree,
        node: NodeId,
        environment: MediaEnvironment,
        depth: usize,
    ) -> HashMap<String, String> {
        if depth > 64 {
            return HashMap::new();
        }

        let mut winners: HashMap<String, Winner> = HashMap::new();
        let mut candidate_indexes = self.candidates(dom, node);
        candidate_indexes.sort_unstable();
        candidate_indexes.dedup();
        // Reuse selector caches across every candidate for this element. A
        // production bundle can contribute thousands of candidates; creating
        // fresh relative-selector caches for each one dominated style queries.
        let mut selector_caches = selectors::context::SelectorCaches::default();
        let mut matching_context = MatchingContext::new(
            MatchingMode::Normal,
            None,
            &mut selector_caches,
            QuirksMode::NoQuirks,
            NeedsSelectorFlags::No,
            MatchingForInvalidation::No,
        );

        for index in candidate_indexes {
            let Some(rule) = self.rules.get(index) else {
                continue;
            };
            if !self.sheet_active(rule.sheet_id)
                || !rule
                    .media
                    .iter()
                    .all(|query| media_matches(query, environment))
                || !selectors::matching::matches_selector_list(
                    &rule.selector,
                    &DomElement::new(dom, node),
                    &mut matching_context,
                )
            {
                continue;
            }
            for declaration in &rule.declarations {
                apply_winner(
                    &mut winners,
                    declaration,
                    rule.specificity,
                    rule.source_order,
                );
            }
        }
        if let Some(style) = dom
            .with_node(node, |element| {
                element.get_attribute("style").map(str::to_string)
            })
            .flatten()
        {
            for declaration in parse_declarations(&style) {
                apply_winner(&mut winners, &declaration, u32::MAX - 1, u64::MAX - 1);
            }
        }

        let mut values: HashMap<String, String> = winners
            .into_iter()
            .map(|(name, winner)| (name, winner.value))
            .collect();

        if let Some(parent) = dom.with_node(node, |entry| entry.parent).flatten() {
            let parent_is_element = dom
                .with_node(parent, |entry| entry.is_element())
                .unwrap_or(false);
            if parent_is_element {
                let inherited = self.compute_uncached(dom, parent, environment, depth + 1);
                for (name, value) in inherited {
                    if (name.starts_with("--") || is_inherited_property(&name))
                        && !values.contains_key(&name)
                    {
                        values.insert(name, value);
                    }
                }
            }
        }
        let variables: HashMap<String, String> = values
            .iter()
            .filter(|(name, _)| name.starts_with("--"))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        let mut resolved_variables: HashMap<String, Option<String>> = HashMap::new();
        for (name, value) in values.iter_mut() {
            // Custom properties are a token stream and are resolved when a
            // normal declaration references them. Eagerly expanding every
            // root variable makes design-system bundles quadratic while doing
            // work no caller requested.
            if !name.starts_with("--") {
                let resolved = resolve_vars(
                    value,
                    &variables,
                    &mut HashSet::new(),
                    &mut resolved_variables,
                    0,
                );
                *value = normalize_value(name, &resolved);
            }
        }
        values
    }

    fn candidates(&self, dom: &DomTree, node: NodeId) -> Vec<usize> {
        let mut out = self.universal.clone();
        dom.with_node(node, |element| {
            if let Some(id) = element.get_attribute("id") {
                if let Some(indexes) = self.by_id.get(id) {
                    out.extend(indexes.iter().copied());
                }
            }
            if let Some(classes) = element.get_attribute("class") {
                for class_name in classes.split_whitespace() {
                    if let Some(indexes) = self.by_class.get(class_name) {
                        out.extend(indexes.iter().copied());
                    }
                }
            }
            if let Some(name) = element.as_element() {
                if let Some(indexes) = self.by_tag.get(name.local.as_ref()) {
                    out.extend(indexes.iter().copied());
                }
            }
        });
        out
    }

    fn sheet_active(&self, sheet_id: u32) -> bool {
        self.sheets
            .iter()
            .find(|sheet| sheet.info.id == sheet_id)
            .map(|sheet| sheet.active && !sheet.info.disabled)
            .unwrap_or(false)
    }

    fn remove_sheet(&mut self, sheet_id: u32) {
        self.sheets.retain(|sheet| sheet.info.id != sheet_id);
        for imports in self.imports_by_owner.values_mut() {
            imports.retain(|id| *id != sheet_id);
        }
        self.decoded_bytes = self.sheets.iter().map(|sheet| sheet.decoded_bytes).sum();
        self.rebuild_rule_indexes();
    }

    fn set_sheet_active(&mut self, sheet_id: u32, active: bool) {
        if let Some(sheet) = self
            .sheets
            .iter_mut()
            .find(|sheet| sheet.info.id == sheet_id)
        {
            sheet.active = active;
            self.invalidate();
        }
    }

    fn rebuild_rule_indexes(&mut self) {
        let active_ids: HashSet<u32> = self.sheets.iter().map(|sheet| sheet.info.id).collect();
        self.rules
            .retain(|rule| active_ids.contains(&rule.sheet_id));
        self.by_id.clear();
        self.by_class.clear();
        self.by_tag.clear();
        self.universal.clear();
        for (index, rule) in self.rules.iter().enumerate() {
            match &rule.key {
                RuleKey::Id(value) => self.by_id.entry(value.clone()).or_default().push(index),
                RuleKey::Class(value) => {
                    self.by_class.entry(value.clone()).or_default().push(index)
                }
                RuleKey::Tag(value) => self.by_tag.entry(value.clone()).or_default().push(index),
                RuleKey::Universal => self.universal.push(index),
            }
        }
        self.invalidate();
    }

    pub fn invalidate(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.cache.clear();
    }
}

fn apply_winner(
    winners: &mut HashMap<String, Winner>,
    declaration: &Declaration,
    specificity: u32,
    order: u64,
) {
    for expanded in expand_declaration(declaration) {
        let candidate = Winner {
            value: expanded.value,
            important: expanded.important,
            specificity,
            order,
        };
        let replace = winners
            .get(&expanded.name)
            .map(|current| {
                (candidate.important, candidate.specificity, candidate.order)
                    >= (current.important, current.specificity, current.order)
            })
            .unwrap_or(true);
        if replace {
            winners.insert(expanded.name, candidate);
        }
    }
}

struct SheetParser {
    sheet_id: u32,
    next_order: u64,
    initial_rule_count: usize,
    compiled: Vec<CompiledRule>,
    css_rules: Vec<CssRuleInfo>,
    hit_rule_limit: bool,
}

impl SheetParser {
    fn new(sheet_id: u32, next_order: u64, initial_rule_count: usize) -> Self {
        Self {
            sheet_id,
            next_order,
            initial_rule_count,
            compiled: Vec::new(),
            css_rules: Vec::new(),
            hit_rule_limit: false,
        }
    }

    fn parse(&mut self, css: &str, conditions: Vec<String>) {
        if self.initial_rule_count + self.compiled.len() >= MAX_RULES {
            self.hit_rule_limit = true;
            return;
        }
        let mut input = ParserInput::new(css);
        let mut input = Parser::new(&mut input);
        let mut capture = CaptureRuleParser;
        for result in StyleSheetParser::new(&mut input, &mut capture) {
            if self.initial_rule_count + self.compiled.len() >= MAX_RULES {
                self.hit_rule_limit = true;
                break;
            }
            let Ok(rule) = result else { continue };
            match rule {
                CapturedRule::Style { selector, body } => {
                    let css_text = format!("{} {{{}}}", selector.trim(), body.trim());
                    self.css_rules.push(CssRuleInfo {
                        css_text,
                        rule_type: 1,
                    });
                    let declarations = parse_declarations(&body);
                    for selector_text in split_selector_list(&selector) {
                        let Ok(parsed) = parse_selector(selector_text) else {
                            continue;
                        };
                        let Some(selector) = parsed.slice().first() else {
                            continue;
                        };
                        self.next_order += 1;
                        self.compiled.push(CompiledRule {
                            specificity: selector.specificity(),
                            selector: parsed,
                            key: selector_key(selector_text),
                            declarations: declarations.clone(),
                            media: conditions.clone(),
                            sheet_id: self.sheet_id,
                            source_order: self.next_order,
                        });
                    }
                }
                CapturedRule::At {
                    name,
                    prelude,
                    body,
                } => {
                    let lower = name.to_ascii_lowercase();
                    let css_text = if let Some(body) = &body {
                        format!("@{} {} {{{}}}", name, prelude.trim(), body.trim())
                    } else {
                        format!("@{} {};", name, prelude.trim())
                    };
                    let rule_type = match lower.as_str() {
                        "import" => 3,
                        "media" => 4,
                        "font-face" => 5,
                        "keyframes" | "-webkit-keyframes" => 7,
                        "supports" => 12,
                        "layer" => 0,
                        _ => 0,
                    };
                    self.css_rules.push(CssRuleInfo {
                        css_text,
                        rule_type,
                    });
                    if let Some(body) = body {
                        match lower.as_str() {
                            "media" => {
                                let mut nested = conditions.clone();
                                nested.push(prelude);
                                self.parse(&body, nested);
                            }
                            "supports" if supports_condition(&prelude) => {
                                self.parse(&body, conditions.clone());
                            }
                            "layer" | "scope" => self.parse(&body, conditions.clone()),
                            _ => {}
                        }
                    }
                }
            }
        }
    }
}

enum CapturedRule {
    Style {
        selector: String,
        body: String,
    },
    At {
        name: String,
        prelude: String,
        body: Option<String>,
    },
}

struct CaptureRuleParser;

impl<'i> QualifiedRuleParser<'i> for CaptureRuleParser {
    type Prelude = String;
    type QualifiedRule = CapturedRule;
    type Error = ();

    fn parse_prelude<'t>(
        &mut self,
        input: &mut Parser<'i, 't>,
    ) -> Result<String, ParseError<'i, ()>> {
        let start = input.position();
        while input.next_including_whitespace().is_ok() {}
        Ok(input.slice_from(start).to_string())
    }

    fn parse_block<'t>(
        &mut self,
        selector: String,
        _start: &ParserState,
        input: &mut Parser<'i, 't>,
    ) -> Result<CapturedRule, ParseError<'i, ()>> {
        let start = input.position();
        while input.next_including_whitespace().is_ok() {}
        Ok(CapturedRule::Style {
            selector,
            body: input.slice_from(start).to_string(),
        })
    }
}

impl<'i> AtRuleParser<'i> for CaptureRuleParser {
    type Prelude = (String, String);
    type AtRule = CapturedRule;
    type Error = ();

    fn parse_prelude<'t>(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i, 't>,
    ) -> Result<Self::Prelude, ParseError<'i, ()>> {
        let start = input.position();
        while input.next_including_whitespace().is_ok() {}
        Ok((name.to_string(), input.slice_from(start).to_string()))
    }

    fn rule_without_block(
        &mut self,
        (name, prelude): Self::Prelude,
        _start: &ParserState,
    ) -> Result<Self::AtRule, ()> {
        Ok(CapturedRule::At {
            name,
            prelude,
            body: None,
        })
    }

    fn parse_block<'t>(
        &mut self,
        (name, prelude): Self::Prelude,
        _start: &ParserState,
        input: &mut Parser<'i, 't>,
    ) -> Result<Self::AtRule, ParseError<'i, ()>> {
        let start = input.position();
        while input.next_including_whitespace().is_ok() {}
        Ok(CapturedRule::At {
            name,
            prelude,
            body: Some(input.slice_from(start).to_string()),
        })
    }
}

struct DeclarationCapture;

impl<'i> DeclarationParser<'i> for DeclarationCapture {
    type Declaration = Declaration;
    type Error = ();

    fn parse_value<'t>(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i, 't>,
    ) -> Result<Declaration, ParseError<'i, ()>> {
        let start = input.position();
        while input.next_including_whitespace().is_ok() {}
        let mut value = input.slice_from(start).trim().to_string();
        let important = strip_important(&mut value);
        if value.is_empty() {
            return Err(input.new_error(BasicParseErrorKind::UnexpectedToken(Token::Ident(name))));
        }
        Ok(Declaration {
            // CSS property names are ASCII case-insensitive, but custom
            // property names are case-sensitive (CSS Variables section 2).
            name: if name.starts_with("--") {
                name.to_string()
            } else {
                name.to_ascii_lowercase()
            },
            value,
            important,
        })
    }
}

impl<'i> AtRuleParser<'i> for DeclarationCapture {
    type Prelude = ();
    type AtRule = Declaration;
    type Error = ();
}

impl<'i> QualifiedRuleParser<'i> for DeclarationCapture {
    type Prelude = ();
    type QualifiedRule = Declaration;
    type Error = ();
}

impl<'i> RuleBodyItemParser<'i, Declaration, ()> for DeclarationCapture {
    fn parse_declarations(&self) -> bool {
        true
    }
    fn parse_qualified(&self) -> bool {
        false
    }
}

fn parse_declarations(input: &str) -> Vec<Declaration> {
    let mut parser_input = ParserInput::new(input);
    let mut parser = Parser::new(&mut parser_input);
    let mut capture = DeclarationCapture;
    RuleBodyParser::new(&mut parser, &mut capture)
        .filter_map(Result::ok)
        .collect()
}

fn strip_important(value: &mut String) -> bool {
    let trimmed = value.trim_end();
    let Some(bang) = trimmed.rfind('!') else {
        return false;
    };
    if trimmed[bang + 1..].trim().eq_ignore_ascii_case("important") {
        value.truncate(bang);
        *value = value.trim_end().to_string();
        true
    } else {
        false
    }
}

fn split_selector_list(input: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut round = 0u32;
    let mut square = 0u32;
    let mut quote = None;
    let mut escaped = false;
    for (index, ch) in input.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '(' => round += 1,
            ')' => round = round.saturating_sub(1),
            '[' => square += 1,
            ']' => square = square.saturating_sub(1),
            ',' if round == 0 && square == 0 => {
                let selector = input[start..index].trim();
                if !selector.is_empty() {
                    out.push(selector);
                }
                start = index + ch.len_utf8();
            }
            _ => {}
        }
    }
    let selector = input[start..].trim();
    if !selector.is_empty() {
        out.push(selector);
    }
    out
}

fn selector_key(selector: &str) -> RuleKey {
    let tail = selector
        .rsplit(|ch: char| ch == '>' || ch == '+' || ch == '~' || ch.is_whitespace())
        .find(|part| !part.is_empty())
        .unwrap_or(selector);
    if let Some(index) = tail.rfind('#') {
        let value = identifier_prefix(&tail[index + 1..]);
        if !value.is_empty() {
            return RuleKey::Id(value.to_string());
        }
    }
    if let Some(index) = tail.rfind('.') {
        let value = identifier_prefix(&tail[index + 1..]);
        if !value.is_empty() {
            return RuleKey::Class(value.to_string());
        }
    }
    let tag = identifier_prefix(tail.trim_start_matches('*'));
    if !tag.is_empty()
        && tail
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphabetic())
    {
        RuleKey::Tag(tag.to_ascii_lowercase())
    } else {
        RuleKey::Universal
    }
}

fn identifier_prefix(input: &str) -> &str {
    let end = input
        .char_indices()
        .find(|(_, ch)| !(ch.is_ascii_alphanumeric() || *ch == '-' || *ch == '_'))
        .map(|(index, _)| index)
        .unwrap_or(input.len());
    &input[..end]
}

fn expand_declaration(declaration: &Declaration) -> Vec<Declaration> {
    if declaration.name == "border" {
        let values = split_component_whitespace(&declaration.value);
        let mut width = "medium".to_string();
        let mut style = "none".to_string();
        let mut color = "currentcolor".to_string();
        for value in values {
            if matches!(
                value.to_ascii_lowercase().as_str(),
                "none" | "hidden" | "dotted" | "dashed" | "solid" | "double" | "groove"
                    | "ridge" | "inset" | "outset"
            ) {
                style = value;
            } else if matches!(value.as_str(), "thin" | "medium" | "thick")
                || value == "0"
                || value.chars().next().is_some_and(|ch| ch.is_ascii_digit() || ch == '.')
            {
                width = value;
            } else {
                color = value;
            }
        }
        let mut out = vec![declaration.clone()];
        for side in ["top", "right", "bottom", "left"] {
            for (kind, value) in [
                ("width", width.as_str()),
                ("style", style.as_str()),
                ("color", color.as_str()),
            ] {
                out.push(Declaration {
                    name: format!("border-{side}-{kind}"),
                    value: value.to_string(),
                    important: declaration.important,
                });
            }
        }
        return out;
    }
    let names: &[&str] = match declaration.name.as_str() {
        "margin" => &["margin-top", "margin-right", "margin-bottom", "margin-left"],
        "padding" => &[
            "padding-top",
            "padding-right",
            "padding-bottom",
            "padding-left",
        ],
        "inset" => &["top", "right", "bottom", "left"],
        "border-width" => &[
            "border-top-width",
            "border-right-width",
            "border-bottom-width",
            "border-left-width",
        ],
        "border-style" => &[
            "border-top-style",
            "border-right-style",
            "border-bottom-style",
            "border-left-style",
        ],
        "border-color" => &[
            "border-top-color",
            "border-right-color",
            "border-bottom-color",
            "border-left-color",
        ],
        "gap" => &["row-gap", "column-gap"],
        _ => return vec![declaration.clone()],
    };
    let values = split_component_whitespace(&declaration.value);
    if values.is_empty() {
        return vec![declaration.clone()];
    }
    let expanded_values = match names.len() {
        2 => vec![
            values[0].clone(),
            values.get(1).cloned().unwrap_or_else(|| values[0].clone()),
        ],
        4 => match values.len() {
            1 => vec![values[0].clone(); 4],
            2 => vec![
                values[0].clone(),
                values[1].clone(),
                values[0].clone(),
                values[1].clone(),
            ],
            3 => vec![
                values[0].clone(),
                values[1].clone(),
                values[2].clone(),
                values[1].clone(),
            ],
            _ => values.into_iter().take(4).collect(),
        },
        _ => Vec::new(),
    };
    let mut out = vec![declaration.clone()];
    out.extend(
        names
            .iter()
            .zip(expanded_values)
            .map(|(name, value)| Declaration {
                name: (*name).to_string(),
                value,
                important: declaration.important,
            }),
    );
    out
}

fn split_component_whitespace(input: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut current = String::new();
    let mut depth = 0u32;
    for ch in input.chars() {
        match ch {
            '(' => {
                depth += 1;
                current.push(ch);
            }
            ')' => {
                depth = depth.saturating_sub(1);
                current.push(ch);
            }
            ch if ch.is_whitespace() && depth == 0 => {
                if !current.is_empty() {
                    values.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        values.push(current);
    }
    values
}

fn resolve_vars(
    input: &str,
    variables: &HashMap<String, String>,
    resolving: &mut HashSet<String>,
    resolved: &mut HashMap<String, Option<String>>,
    depth: usize,
) -> String {
    if depth > 32 {
        return String::new();
    }
    let mut output = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("var(") {
        output.push_str(&rest[..start]);
        let after = &rest[start + 4..];
        let Some(end) = find_matching_paren(after) else {
            output.push_str(&rest[start..]);
            return output;
        };
        let args = &after[..end];
        let (name, fallback) = split_var_args(args);
        let name = name.trim();
        let variable_value = if !name.starts_with("--") {
            None
        } else if let Some(value) = resolved.get(name) {
            value.clone()
        } else if resolving.insert(name.to_string()) {
            let value = variables.get(name).and_then(|value| {
                let value = resolve_vars(value, variables, resolving, resolved, depth + 1);
                (!value.is_empty()).then_some(value)
            });
            resolving.remove(name);
            resolved.insert(name.to_string(), value.clone());
            value
        } else {
            None
        };
        let replacement = variable_value.unwrap_or_else(|| {
            fallback
                .map(|value| resolve_vars(value, variables, resolving, resolved, depth + 1))
                .unwrap_or_default()
        });
        output.push_str(&replacement);
        rest = &after[end + 1..];
    }
    output.push_str(rest);
    output.trim().to_string()
}

fn find_matching_paren(input: &str) -> Option<usize> {
    let mut depth = 0u32;
    for (index, ch) in input.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' if depth == 0 => return Some(index),
            ')' => depth -= 1,
            _ => {}
        }
    }
    None
}

fn split_var_args(input: &str) -> (&str, Option<&str>) {
    let mut depth = 0u32;
    for (index, ch) in input.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => return (&input[..index], Some(&input[index + 1..])),
            _ => {}
        }
    }
    (input, None)
}

fn normalize_value(property: &str, input: &str) -> String {
    let value = input.trim();
    if value == "0" && is_length_property(property) {
        return "0px".to_string();
    }
    if is_length_property(property) {
        for (unit, factor) in [
            ("in", 96.0),
            ("cm", 96.0 / 2.54),
            ("mm", 96.0 / 25.4),
            ("q", 96.0 / 101.6),
            ("pt", 4.0 / 3.0),
            ("pc", 16.0),
        ] {
            if let Some(number) = value.strip_suffix(unit).and_then(|raw| raw.parse::<f64>().ok()) {
                let pixels = number * factor;
                return if pixels.fract().abs() < f64::EPSILON {
                    format!("{}px", pixels as i64)
                } else {
                    let number = format!("{pixels:.4}");
                    format!("{}px", number.trim_end_matches('0').trim_end_matches('.'))
                };
            }
        }
    }
    match value.to_ascii_lowercase().as_str() {
        "transparent" => "rgba(0, 0, 0, 0)".to_string(),
        "black" => "rgb(0, 0, 0)".to_string(),
        "white" => "rgb(255, 255, 255)".to_string(),
        "red" => "rgb(255, 0, 0)".to_string(),
        "green" => "rgb(0, 128, 0)".to_string(),
        "blue" => "rgb(0, 0, 255)".to_string(),
        _ => value.to_string(),
    }
}

fn is_length_property(name: &str) -> bool {
    matches!(name, "top" | "right" | "bottom" | "left")
        || name.contains("width")
        || name.contains("height")
        || name.contains("margin")
        || name.contains("padding")
        || name.contains("inset")
        || name.contains("gap")
        || name.contains("radius")
        || name.ends_with("-size")
        || name.ends_with("-spacing")
        || name.ends_with("-offset")
}

fn is_inherited_property(name: &str) -> bool {
    matches!(
        name,
        "color"
            | "cursor"
            | "direction"
            | "font"
            | "font-family"
            | "font-size"
            | "font-style"
            | "font-variant"
            | "font-weight"
            | "letter-spacing"
            | "line-height"
            | "list-style"
            | "text-align"
            | "text-indent"
            | "text-transform"
            | "visibility"
            | "white-space"
            | "word-spacing"
    )
}

fn supports_condition(input: &str) -> bool {
    let value = input.trim();
    value.starts_with('(') && value.contains(':') && value.ends_with(')')
}

/// Extract top-level `@import` targets without retaining or evaluating the
/// stylesheet. The browser loader uses this only in compute mode; drop mode
/// intentionally performs no CSS parsing at all.
pub fn extract_imports(css: &str) -> Vec<(String, String)> {
    let mut input = ParserInput::new(css);
    let mut input = Parser::new(&mut input);
    let mut capture = CaptureRuleParser;
    StyleSheetParser::new(&mut input, &mut capture)
        .filter_map(Result::ok)
        .filter_map(|rule| match rule {
            CapturedRule::At { name, prelude, .. } if name.eq_ignore_ascii_case("import") => {
                parse_import_prelude(&prelude)
            }
            _ => None,
        })
        .collect()
}

fn parse_import_prelude(input: &str) -> Option<(String, String)> {
    let input = input.trim();
    if let Some(rest) = input.strip_prefix("url(") {
        let end = rest.find(')')?;
        let url = rest[..end].trim().trim_matches(['\'', '"']).to_string();
        let media = rest[end + 1..].trim().to_string();
        return (!url.is_empty()).then_some((url, media));
    }
    let quote = input.chars().next()?;
    if quote == '\'' || quote == '"' {
        let rest = &input[quote.len_utf8()..];
        let end = rest.find(quote)?;
        let url = rest[..end].to_string();
        let media = rest[end + quote.len_utf8()..].trim().to_string();
        return (!url.is_empty()).then_some((url, media));
    }
    None
}

fn media_matches(input: &str, environment: MediaEnvironment) -> bool {
    let query = input.to_ascii_lowercase().replace(char::is_whitespace, "");
    if query.is_empty() || query == "all" || query == "screen" {
        return true;
    }
    if query.contains("print") && !query.contains("notprint") {
        return false;
    }
    for part in query.split(',') {
        let mut matched = true;
        for condition in part.split("and") {
            let condition = condition.trim_matches(|ch| ch == '(' || ch == ')');
            if let Some(value) = condition.strip_prefix("min-width:").and_then(parse_px) {
                matched &= environment.width >= value;
            } else if let Some(value) = condition.strip_prefix("max-width:").and_then(parse_px) {
                matched &= environment.width <= value;
            } else if let Some(value) = condition.strip_prefix("min-height:").and_then(parse_px) {
                matched &= environment.height >= value;
            } else if let Some(value) = condition.strip_prefix("max-height:").and_then(parse_px) {
                matched &= environment.height <= value;
            } else if condition == "prefers-color-scheme:dark" {
                matched &= environment.dark;
            } else if condition == "prefers-color-scheme:light" {
                matched &= !environment.dark;
            } else if condition == "prefers-reduced-motion:reduce" {
                matched &= environment.reduced_motion;
            } else if condition == "prefers-reduced-motion:no-preference" {
                matched &= !environment.reduced_motion;
            } else if condition == "hover:hover" || condition == "any-hover:hover" {
                matched &= environment.hover;
            } else if condition == "pointer:fine" || condition == "any-pointer:fine" {
                matched &= environment.pointer_fine;
            } else if matches!(condition, "screen" | "all" | "" | "color") {
            } else {
                matched = false;
            }
        }
        if matched {
            return true;
        }
    }
    false
}

fn parse_px(input: &str) -> Option<u32> {
    input.strip_suffix("px")?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_html;

    #[test]
    fn cascade_specificity_important_inline_and_variables() {
        let dom = parse_html(
            "<html><body><div id='target' class='box' style='padding: 3px'></div></body></html>",
        );
        let node = dom.query_selector("#target").unwrap().unwrap();
        let mut engine = StyleEngine::new();
        engine.register_stylesheet(
            None,
            None,
            String::new(),
            true,
            r#"
            :root { --brand: blue; color: black; }
            div { color: red; margin: 1px 2px; }
            .box { color: var(--brand); }
            #target { color: green !important; }
        "#,
        );
        let style = engine.compute_style(&dom, node, MediaEnvironment::default());
        assert_eq!(
            style.get("color").map(String::as_str),
            Some("rgb(0, 128, 0)")
        );
        assert_eq!(style.get("margin-left").map(String::as_str), Some("2px"));
        assert_eq!(style.get("padding-top").map(String::as_str), Some("3px"));
        assert_eq!(style.get("--brand").map(String::as_str), Some("blue"));
    }

    #[test]
    fn media_and_parser_recovery_are_local() {
        let dom = parse_html("<html><body><p class='x'>x</p></body></html>");
        let node = dom.query_selector("p").unwrap().unwrap();
        let mut engine = StyleEngine::new();
        engine.register_stylesheet(
            None,
            None,
            String::new(),
            true,
            r#"
            .x:future-pseudo { color: red }
            @media (min-width: 1000px) { .x { display: flex } }
            broken !! { nope }
            .x { opacity: .5 }
        "#,
        );
        let style = engine.compute_style(&dom, node, MediaEnvironment::default());
        assert_eq!(style.get("display").map(String::as_str), Some("flex"));
        assert_eq!(style.get("opacity").map(String::as_str), Some(".5"));
        assert!(!style.contains_key("color"));
    }

    #[test]
    fn custom_property_cycles_use_fallback() {
        let dom = parse_html("<html><body><div></div></body></html>");
        let node = dom.query_selector("div").unwrap().unwrap();
        let mut engine = StyleEngine::new();
        engine.register_stylesheet(
            None,
            None,
            String::new(),
            true,
            ":root { --a: var(--b); --b: var(--a); } div { color: var(--a, red); }",
        );
        let style = engine.compute_style(&dom, node, MediaEnvironment::default());
        assert_eq!(
            style.get("color").map(String::as_str),
            Some("rgb(255, 0, 0)")
        );
    }

    #[test]
    fn custom_property_names_remain_case_sensitive() {
        let dom = parse_html("<html><body><button class='button'></button></body></html>");
        let node = dom.query_selector("button").unwrap().unwrap();
        let mut engine = StyleEngine::new();
        engine.register_stylesheet(
            None,
            None,
            String::new(),
            true,
            ".button { --button--Display: inline-flex; display: var(--button--Display); }",
        );
        let style = engine.compute_style(&dom, node, MediaEnvironment::default());
        assert_eq!(
            style.get("--button--Display").map(String::as_str),
            Some("inline-flex")
        );
        assert_eq!(style.get("display").map(String::as_str), Some("inline-flex"));
    }

    #[test]
    fn shorthands_indexing_and_invalidation_work_together() {
        let dom = parse_html("<html><body><div id='target' class='card'></div></body></html>");
        let node = dom.query_selector("#target").unwrap().unwrap();
        let mut engine = StyleEngine::new();
        let sheet = engine
            .register_stylesheet(
                None,
                None,
                String::new(),
                true,
                ".unrelated { color: blue } .card { padding: 1px 2px 3px 4px; inset: 0; gap: 5px 6px; opacity: 0; width: 72pt; border: 2pt solid red }",
            )
            .unwrap();
        assert!(engine.candidates(&dom, node).len() < engine.rules.len());
        let style = engine.compute_style(&dom, node, MediaEnvironment::default());
        assert_eq!(style.get("padding-left").map(String::as_str), Some("4px"));
        assert_eq!(style.get("bottom").map(String::as_str), Some("0px"));
        assert_eq!(style.get("column-gap").map(String::as_str), Some("6px"));
        assert_eq!(style.get("opacity").map(String::as_str), Some("0"));
        assert_eq!(style.get("width").map(String::as_str), Some("96px"));
        assert_eq!(style.get("border-left-width").map(String::as_str), Some("2.6667px"));
        assert_eq!(style.get("border-left-style").map(String::as_str), Some("solid"));
        assert_eq!(style.get("border-left-color").map(String::as_str), Some("rgb(255, 0, 0)"));

        assert!(engine.set_disabled(sheet, true));
        assert!(engine
            .compute_style(&dom, node, MediaEnvironment::default())
            .get("padding-left")
            .is_none());
    }

    #[test]
    fn resource_limits_truncate_safely() {
        let mut bytes = StyleEngine::new();
        bytes.decoded_bytes = MAX_CSS_BYTES;
        let byte_limited = bytes
            .register_stylesheet(None, None, String::new(), true, ".x{}")
            .expect("a truncated sheet should retain CSSOM metadata");
        assert_eq!(bytes.sheet_rules(byte_limited).unwrap().len(), 0);
        assert!(bytes.truncated);

        let mut sheets = StyleEngine::new();
        for _ in 0..MAX_SHEETS {
            assert!(sheets
                .register_stylesheet(None, None, String::new(), true, "")
                .is_some());
        }
        assert!(sheets
            .register_stylesheet(None, None, String::new(), true, "")
            .is_none());
        assert!(sheets.truncated);

        let mut parser = SheetParser::new(1, 0, MAX_RULES);
        parser.parse(".x { color: red }", Vec::new());
        assert!(parser.hit_rule_limit);
        assert!(parser.compiled.is_empty());
        assert_eq!(MAX_IMPORT_DEPTH, 8);
    }

    #[test]
    fn imported_sheets_follow_owner_lifetime() {
        let dom = parse_html("<html><head><link id='owner'></head><body></body></html>");
        let owner = dom.query_selector("#owner").unwrap().unwrap();
        let body = dom.query_selector("body").unwrap().unwrap();
        let mut engine = StyleEngine::new();
        engine.register_import_stylesheet(
            owner,
            "https://example.test/import.css".to_string(),
            String::new(),
            true,
            "body { color: red }",
        );
        engine.register_stylesheet(
            Some(owner),
            Some("https://example.test/root.css".to_string()),
            String::new(),
            true,
            "body { display: block }",
        );
        assert_eq!(
            engine
                .compute_style(&dom, body, MediaEnvironment::default())
                .get("color")
                .map(String::as_str),
            Some("rgb(255, 0, 0)")
        );
        engine.remove_owner(owner);
        assert!(engine.sheet_infos().is_empty());
        assert!(engine
            .compute_style(&dom, body, MediaEnvironment::default())
            .is_empty());
    }
}
