//! Autonomous AI-agent action protocol and semantic DOM distillation for Obscura Next.

use obscura_dom::{DomTree, NodeId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Instant;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum AgentError {
    #[error("Action target selector '{0}' matched no elements")]
    ElementNotFound(String),

    #[error("Action execution failed: {0}")]
    ExecutionFailed(String),

    #[error("Serialization error: {0}")]
    Serialization(String),
}

/// High-level actions an autonomous agent can instruct the browser to perform.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum AgentAction {
    Navigate {
        url: String,
        #[serde(default)]
        wait_until: Option<String>,
    },
    Click {
        selector: String,
    },
    Type {
        selector: String,
        text: String,
        #[serde(default)]
        clear: bool,
    },
    Select {
        selector: String,
        value: String,
    },
    Scroll {
        #[serde(default)]
        x: f32,
        #[serde(default)]
        y: f32,
    },
    Wait {
        milliseconds: u64,
    },
    ExtractSemanticTree,
    ExtractMarkdown,
}

/// Structured outcome of an executed agent action.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentActionResult {
    pub action: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    pub elapsed_ms: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl AgentActionResult {
    pub fn success(action: &str, payload: Option<serde_json::Value>, start: Instant) -> Self {
        Self {
            action: action.to_string(),
            success: true,
            payload,
            elapsed_ms: start.elapsed().as_secs_f64() * 1000.0,
            error: None,
        }
    }

    pub fn failure(action: &str, err: String, start: Instant) -> Self {
        Self {
            action: action.to_string(),
            success: false,
            payload: None,
            elapsed_ms: start.elapsed().as_secs_f64() * 1000.0,
            error: Some(err),
        }
    }
}

/// Token-optimized semantic representation of a DOM node for LLM inference.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SemanticNode {
    pub id: String,
    pub tag: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub text: String,
    pub selector: String,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub attributes: HashMap<String, String>,
    pub is_interactive: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<SemanticNode>,
}

/// Walks a DOM tree and distills it into an AI-ready semantic structure.
pub struct SemanticDistiller;

impl SemanticDistiller {
    pub fn distill(dom: &DomTree) -> Option<SemanticNode> {
        let doc = dom.document();
        let mut counter = 1usize;
        for child in dom.children(doc) {
            if let Some(node) = Self::distill_node(dom, child, &mut counter) {
                return Some(node);
            }
        }
        None
    }

    fn distill_node(dom: &DomTree, node_id: NodeId, counter: &mut usize) -> Option<SemanticNode> {
        let (tag_lower, attrs) = dom.with_node(node_id, |n| {
            let tag = n.as_element().map(|name| name.local.to_string())?;
            let mut attrs = HashMap::new();
            if let Some(id_attr) = n.get_attribute("id") {
                attrs.insert("id".to_string(), id_attr.to_string());
            }
            if let Some(href) = n.get_attribute("href") {
                attrs.insert("href".to_string(), href.to_string());
            }
            if let Some(role) = n.get_attribute("role") {
                attrs.insert("role".to_string(), role.to_string());
            }
            Some((tag.to_ascii_lowercase(), attrs))
        }).flatten()?;

        // Skip non-visible, non-semantic tags
        if matches!(
            tag_lower.as_str(),
            "script" | "style" | "noscript" | "meta" | "link" | "svg" | "path"
        ) {
            return None;
        }

        let is_interactive = matches!(
            tag_lower.as_str(),
            "a" | "button" | "input" | "select" | "textarea"
        );

        let ref_id = format!("@e{}", *counter);
        *counter += 1;

        let selector = if let Some(id) = attrs.get("id") {
            format!("#{id}")
        } else {
            tag_lower.clone()
        };

        let mut children = Vec::new();
        for child_id in dom.children(node_id) {
            if let Some(child_node) = Self::distill_node(dom, child_id, counter) {
                children.push(child_node);
            }
        }

        let direct_text = dom.text_content(node_id).trim().to_string();

        Some(SemanticNode {
            id: ref_id,
            tag: tag_lower,
            role: attrs.get("role").cloned(),
            text: direct_text,
            selector,
            attributes: attrs,
            is_interactive,
            children,
        })
    }

    /// Renders semantic node hierarchy into compact Markdown for LLM prompt context.
    pub fn to_compact_markdown(node: &SemanticNode) -> String {
        let mut out = String::new();
        Self::render_markdown(node, 0, &mut out);
        out
    }

    fn render_markdown(node: &SemanticNode, depth: usize, out: &mut String) {
        let indent = "  ".repeat(depth);
        if node.is_interactive {
            out.push_str(&format!(
                "{indent}- [{} ({})] \"{}\" -> {}\n",
                node.id, node.tag, node.text, node.selector
            ));
        } else if !node.text.is_empty() && node.children.is_empty() {
            out.push_str(&format!("{indent}- {}\n", node.text));
        }

        for child in &node.children {
            Self::render_markdown(child, depth + 1, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_action_serialization() {
        let action = AgentAction::Click {
            selector: "#submit-btn".to_string(),
        };
        let serialized = serde_json::to_string(&action).unwrap();
        assert!(serialized.contains("\"action\":\"click\""));
        assert!(serialized.contains("#submit-btn"));

        let deserialized: AgentAction = serde_json::from_str(&serialized).unwrap();
        if let AgentAction::Click { selector } = deserialized {
            assert_eq!(selector, "#submit-btn");
        } else {
            panic!("Mismatched deserialization");
        }
    }

    #[test]
    fn test_semantic_distiller() {
        let html = r#"
            <!DOCTYPE html>
            <html>
                <head><script>alert(1);</script><style>body{color:red;}</style></head>
                <body>
                    <h1>Welcome</h1>
                    <p>Some intro text</p>
                    <a id="signup-link" href="/signup">Sign Up</a>
                    <button id="cta-btn">Click Me</button>
                </body>
            </html>
        "#;
        let dom = obscura_dom::parse_html(html);
        let semantic_tree = SemanticDistiller::distill(&dom).expect("valid semantic tree");

        assert_eq!(semantic_tree.tag, "html");
        let md = SemanticDistiller::to_compact_markdown(&semantic_tree);
        assert!(md.contains("Sign Up"));
        assert!(md.contains("Click Me"));
        assert!(md.contains("#signup-link"));
        assert!(md.contains("#cta-btn"));
    }
}
