use obscura_mcp::install;

// ── transform_skill ────────────────────────────────────────────────────────

const SAMPLE_SKILL: &str = r#"---
name: test-skill
description: A test skill
---

Skill content here.
"#;

#[test]
fn skill_no_transform_for_claude() {
    assert_eq!(
        install::transform_skill(SAMPLE_SKILL, "claude"),
        SAMPLE_SKILL
    );
}

#[test]
fn skill_no_transform_for_gemini() {
    assert_eq!(
        install::transform_skill(SAMPLE_SKILL, "gemini"),
        SAMPLE_SKILL
    );
}

#[test]
fn skill_no_transform_for_opencode() {
    assert_eq!(
        install::transform_skill(SAMPLE_SKILL, "opencode"),
        SAMPLE_SKILL
    );
}

#[test]
fn skill_cursor_adds_frontmatter() {
    let result = install::transform_skill(SAMPLE_SKILL, "cursor");
    assert!(result.contains("globs:"));
    assert!(result.contains("alwaysApply: true"));
    assert!(result.contains("Skill content here."));
}

#[test]
fn skill_codex_preserves_frontmatter() {
    let result = install::transform_skill(SAMPLE_SKILL, "codex");
    assert!(result.starts_with("---\n"));
    assert!(result.contains("Skill content here."));
}

#[test]
fn skill_codex_adds_frontmatter_if_missing() {
    let content = "Just body, no frontmatter.\n";
    let result = install::transform_skill(content, "codex");
    assert!(result.starts_with("---\n"));
    assert!(result.contains("Just body, no frontmatter."));
}

// ── transform_agent ────────────────────────────────────────────────────────

const SAMPLE_AGENT: &str = r#"---
name: test-agent
description: A test agent
tools:
  - Bash
  - Read
---

Agent instructions here.
"#;

const AGENT_WITH_MODEL: &str = r#"---
name: test-agent
description: A test agent
model: sonnet
tools:
  - Bash
---

Agent content.
"#;

#[test]
fn agent_no_transform_for_claude() {
    assert_eq!(
        install::transform_agent(SAMPLE_AGENT, "claude"),
        SAMPLE_AGENT
    );
}

#[test]
fn agent_no_transform_for_gemini() {
    assert_eq!(
        install::transform_agent(SAMPLE_AGENT, "gemini"),
        SAMPLE_AGENT
    );
}

#[test]
fn agent_cursor_adds_model_if_missing() {
    let result = install::transform_agent(SAMPLE_AGENT, "cursor");
    assert!(result.contains("model: inherit"));
}

#[test]
fn agent_cursor_keeps_existing_model() {
    let result = install::transform_agent(AGENT_WITH_MODEL, "cursor");
    assert!(result.contains("model: sonnet"));
    assert!(!result.contains("model: inherit"));
}

#[test]
fn agent_codex_appends_subagent_note() {
    let result = install::transform_agent(SAMPLE_AGENT, "codex");
    assert!(result.contains("Codex Sub-agent"));
}

#[test]
fn agent_opencode_transforms_tools() {
    let result = install::transform_agent(SAMPLE_AGENT, "opencode");
    assert!(result.contains("OpenCode Sub-agent"));
}

// ── ALL_TOOLS registry ────────────────────────────────────────────────────

#[test]
fn all_tools_has_expected_count() {
    assert_eq!(install::ALL_TOOLS.len(), 6);
}

#[test]
fn all_tools_contains_known_ids() {
    let ids: Vec<&str> = install::ALL_TOOLS.iter().map(|(id, _)| *id).collect();
    assert!(ids.contains(&"claude"));
    assert!(ids.contains(&"cursor"));
    assert!(ids.contains(&"gemini"));
    assert!(ids.contains(&"codex"));
    assert!(ids.contains(&"opencode"));
    assert!(ids.contains(&"cline"));
}

#[test]
fn all_tools_have_names() {
    for (_, name) in install::ALL_TOOLS {
        assert!(!name.is_empty(), "tool name should not be empty");
    }
}
