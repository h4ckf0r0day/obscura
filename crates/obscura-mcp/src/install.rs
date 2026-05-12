use std::fs;
use std::path::PathBuf;

use serde_json::{json, Value};

// ── Canonical sources ─────────────────────────────────────────────────────

static SKILL_FETCH: &str = include_str!("../../../registry/skills/obscura-fetch/SKILL.md");
static SKILL_SCRAPE: &str = include_str!("../../../registry/skills/obscura-scrape/SKILL.md");
static SKILL_PIPELINE: &str = include_str!("../../../registry/skills/obscura-pipeline/SKILL.md");
static AGENT_BROWSER: &str = include_str!("../../../registry/agents/obscura-browser.md");

static CANONICAL_SKILLS: &[(&str, &str)] = &[
    ("obscura-fetch", SKILL_FETCH),
    ("obscura-scrape", SKILL_SCRAPE),
    ("obscura-pipeline", SKILL_PIPELINE),
];

static CANONICAL_AGENTS: &[(&str, &str)] = &[("obscura-browser", AGENT_BROWSER)];

// ── Tool registry ────────────────────────────────────────────────────────

pub static ALL_TOOLS: &[(&str, &str)] = &[
    ("claude", "Claude Code"),
    ("cursor", "Cursor"),
    ("gemini", "Gemini CLI"),
    ("codex", "Codex CLI"),
    ("opencode", "OpenCode"),
    ("cline", "Cline"),
];

// ── Tool config ──────────────────────────────────────────────────────────

struct ToolConfig {
    name: &'static str,
    skills_dir: Box<dyn Fn(&str) -> PathBuf>,
    agents_dir: Box<dyn Fn(&str) -> PathBuf>,
    mcp_file: PathBuf,
    mcp_key: &'static str,
    mcp_format: &'static str,
    supports_skills: bool,
    supports_agents: bool,
    supports_mcp: bool,
}

fn home() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/"))
}

fn tool_config(id: &str) -> Option<ToolConfig> {
    match id {
        "claude" => Some(ToolConfig {
            name: "Claude Code",
            skills_dir: Box::new(|s| home().join(format!(".claude/skills/{s}/SKILL.md"))),
            agents_dir: Box::new(|a| home().join(format!(".claude/agents/{a}.md"))),
            mcp_file: home().join(".claude.json"),
            mcp_key: "mcpServers",
            mcp_format: "claude",
            supports_skills: true,
            supports_agents: true,
            supports_mcp: true,
        }),
        "cursor" => Some(ToolConfig {
            name: "Cursor",
            skills_dir: Box::new(|_| home().join(".cursor/rules/obscura-skills.mdc")),
            agents_dir: Box::new(|a| home().join(format!(".cursor/agents/{a}.md"))),
            mcp_file: home().join(".cursor/mcp.json"),
            mcp_key: "mcpServers",
            mcp_format: "standard",
            supports_skills: true,
            supports_agents: true,
            supports_mcp: true,
        }),
        "gemini" => Some(ToolConfig {
            name: "Gemini CLI",
            skills_dir: Box::new(|s| home().join(format!(".gemini/skills/{s}/SKILL.md"))),
            agents_dir: Box::new(|a| home().join(format!(".gemini/agents/{a}.md"))),
            mcp_file: home().join(".gemini/settings.json"),
            mcp_key: "mcpServers",
            mcp_format: "standard",
            supports_skills: true,
            supports_agents: true,
            supports_mcp: true,
        }),
        "codex" => Some(ToolConfig {
            name: "Codex CLI",
            skills_dir: Box::new(|s| home().join(format!(".codex/skills/{s}/SKILL.md"))),
            agents_dir: Box::new(|a| home().join(format!(".codex/skills/{a}/agents/openai.yaml"))),
            mcp_file: home().join(".codex/config.toml"),
            mcp_key: "mcp_servers",
            mcp_format: "toml",
            supports_skills: true,
            supports_agents: true,
            supports_mcp: true,
        }),
        "opencode" => Some(ToolConfig {
            name: "OpenCode",
            skills_dir: Box::new(|s| home().join(format!(".opencode/skills/{s}/SKILL.md"))),
            agents_dir: Box::new(|a| home().join(format!(".config/opencode/agents/{a}.md"))),
            mcp_file: home().join(".config/opencode/opencode.json"),
            mcp_key: "mcp",
            mcp_format: "opencode",
            supports_skills: true,
            supports_agents: true,
            supports_mcp: true,
        }),
        "cline" => Some(ToolConfig {
            name: "Cline",
            skills_dir: Box::new(|s| home().join(format!(".cline/skills/{s}/SKILL.md"))),
            agents_dir: Box::new(|_| PathBuf::new()),
            mcp_file: {
                #[cfg(target_os = "macos")]
                {
                    home().join("Library/Application Support/Code/User/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json")
                }
                #[cfg(not(target_os = "macos"))]
                {
                    home().join(".config/Code/User/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json")
                }
            },
            mcp_key: "mcpServers",
            mcp_format: "standard",
            supports_skills: true,
            supports_agents: false,
            supports_mcp: true,
        }),
        _ => None,
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────

fn ensure_parent(path: &std::path::Path) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
}

fn upsert_file(path: &PathBuf, content: &str) -> bool {
    ensure_parent(path);
    if path.exists() {
        println!("  skip (exists): {}", path.display());
        return false;
    }
    match fs::write(path, content) {
        Ok(()) => {
            println!("  wrote: {}", path.display());
            true
        }
        Err(e) => {
            eprintln!("  error writing {}: {e}", path.display());
            false
        }
    }
}

fn read_json(path: &PathBuf) -> Value {
    if !path.exists() {
        return json!({});
    }
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(json!({}))
}

fn write_json(path: &PathBuf, data: &Value) {
    ensure_parent(path);
    let content = format!("{}\n", serde_json::to_string_pretty(data).unwrap());
    // Atomic write: tmp → rename to avoid partial-write corruption
    let tmp = path.with_file_name(format!(
        ".{}.{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    if fs::write(&tmp, &content).is_ok() {
        let _ = fs::rename(&tmp, path);
    } else {
        // fallback: direct write
        let _ = fs::write(path, &content);
    }
}

// ── MCP injection ────────────────────────────────────────────────────────

fn current_exe_path() -> String {
    std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "obscura-mcp".into())
}

/// Resolve the obscura binary path.
/// Priority: OBSCURA_BIN env → sibling of this exe → "obscura" (PATH fallback).
fn obscura_bin_path() -> String {
    if let Ok(v) = std::env::var("OBSCURA_BIN") {
        if !v.is_empty() {
            return v;
        }
    }
    // Look for `obscura` next to the running `obscura-mcp` binary
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("obscura");
            if candidate.exists() {
                return candidate.to_string_lossy().into_owned();
            }
        }
    }
    "obscura".into()
}

fn inject_mcp(cfg: &ToolConfig) {
    if !cfg.supports_mcp || cfg.mcp_file.as_os_str().is_empty() {
        return;
    }

    let exe = current_exe_path();
    let obscura = obscura_bin_path();

    if cfg.mcp_format == "toml" {
        inject_mcp_toml(&cfg.mcp_file, &exe);
        return;
    }

    let mut data = read_json(&cfg.mcp_file);

    let key = cfg.mcp_key;

    // Build the new entry
    let new_entry = if cfg.mcp_format == "opencode" {
        json!({
            "type": "local",
            "command": [exe, "serve"],
            "environment": { "OBSCURA_BIN": obscura }
        })
    } else {
        json!({
            "command": exe,
            "args": ["serve"],
            "env": { "OBSCURA_BIN": obscura }
        })
    };

    // upsert: skip if identical entry already exists
    if let Some(servers) = data.get(key).and_then(|v| v.get("obscura")) {
        if servers == &new_entry {
            println!("  skip (unchanged): {}", cfg.mcp_file.display());
            return;
        }
    }

    let servers = data
        .as_object_mut()
        .unwrap()
        .entry(key)
        .or_insert_with(|| json!({}));
    servers
        .as_object_mut()
        .unwrap()
        .insert("obscura".into(), new_entry);

    write_json(&cfg.mcp_file, &data);
    println!("  mcp injected: {}", cfg.mcp_file.display());
}

fn remove_mcp(cfg: &ToolConfig) {
    if !cfg.supports_mcp || cfg.mcp_file.as_os_str().is_empty() || !cfg.mcp_file.exists() {
        return;
    }

    if cfg.mcp_format == "toml" {
        remove_mcp_toml(&cfg.mcp_file);
        return;
    }

    let mut data = read_json(&cfg.mcp_file);
    let key = cfg.mcp_key;

    if let Some(obj) = data.as_object_mut() {
        if let Some(servers) = obj.get_mut(key) {
            if let Some(s) = servers.as_object_mut() {
                s.remove("obscura");
                if s.is_empty() {
                    obj.remove(key);
                }
            }
        }
        write_json(&cfg.mcp_file, &data);
        println!("  mcp removed: {}", cfg.mcp_file.display());
    }
}

// ── TOML MCP injection (Codex) ──────────────────────────────────────────

fn remove_toml_sections(content: &str, prefix: &str) -> String {
    let mut result = String::new();
    let mut in_section = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            let section = &trimmed[1..trimmed.len() - 1];
            if section == prefix || section.starts_with(&format!("{prefix}.")) {
                in_section = true;
                continue;
            }
            in_section = false;
        }
        if !in_section && (!result.is_empty() || !line.is_empty()) {
            result.push_str(line);
            result.push('\n');
        }
    }
    result
}

fn inject_mcp_toml(path: &PathBuf, exe: &str) {
    let content = if path.exists() {
        fs::read_to_string(path).unwrap_or_default()
    } else {
        String::new()
    };

    let obscura = obscura_bin_path();
    let new_entry = format!(
        "[mcp_servers.obscura]\ncommand = \"{exe}\"\nargs = [\"serve\"]\nenabled = true\n\n[mcp_servers.obscura.env]\nOBSCURA_BIN = \"{obscura}\"\n"
    );

    // upsert: skip if identical entry already exists
    if content.contains("mcp_servers.obscura") && content.contains(&format!("command = \"{exe}\""))
    {
        println!("  skip (unchanged): {}", path.display());
        return;
    }

    let mut cleaned = remove_toml_sections(&content, "mcp_servers.obscura");
    if !cleaned.ends_with('\n') && !cleaned.is_empty() {
        cleaned.push('\n');
    }
    cleaned.push_str(&new_entry);

    ensure_parent(path);
    let _ = fs::write(path, &cleaned);
    println!("  mcp injected: {}", path.display());
}

fn remove_mcp_toml(path: &PathBuf) {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return,
    };

    let cleaned = remove_toml_sections(&content, "mcp_servers.obscura");
    let trimmed = cleaned.trim_end().to_string();
    if trimmed.is_empty() {
        let _ = fs::remove_file(path);
        println!("  mcp removed (empty): {}", path.display());
    } else {
        let _ = fs::write(path, format!("{trimmed}\n"));
        println!("  mcp removed: {}", path.display());
    }
}

// ── Per-tool transforms ──────────────────────────────────────────────────

pub fn transform_skill(content: &str, tool: &str) -> String {
    match tool {
        "cursor" => content.replacen("---\n", "---\nglobs:\nalwaysApply: true\n", 1),
        "codex" if !content.starts_with("---\n") => format!("---\n---\n\n{content}"),
        _ => content.to_string(),
    }
}

pub fn transform_agent(content: &str, tool: &str) -> String {
    match tool {
        "cursor" => {
            if content.contains("model:") {
                content.to_string()
            } else {
                content.replacen("---\n", "---\nmodel: inherit\n", 1)
            }
        }
        "gemini" => {
            // Gemini CLI uses different tool names
            content
                .replace("  - Bash", "  - run_shell_command")
                .replace("  - Read", "  - read_file")
                .replace("  - Write", "  - write_file")
                .replace("  - Edit", "  - replace_in_file")
                .replace("  - Grep", "  - grep_search")
                .replace("  - Glob", "  - glob")
        }
        "codex" => {
            // Extract frontmatter fields for openai.yaml (Codex Agent Skills spec)
            let name = content
                .lines()
                .find(|l| l.starts_with("name:"))
                .and_then(|l| l.strip_prefix("name:"))
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|| "obscura-browser".to_string());
            let desc = content
                .lines()
                .find(|l| l.starts_with("description:"))
                .and_then(|l| l.strip_prefix("description:"))
                .map(|s| s.trim().trim_matches('"').to_string())
                .unwrap_or_else(|| "Obscura browser agent".to_string());
            format!(
                "name: {name}\ndisplay_name: \"Obscura Browser\"\ndescription: \"{desc}\"\nversion: \"0.1.0\"\ntags:\n  - web-scraping\n  - headless-browser\n  - ai-agent\n\n## Codex Sub-agent\n\nThis agent can be invoked as a Codex sub-agent.\n"
            )
        }
        "opencode" => {
            let re = regex::Regex::new(r"(?m)^(\s*-\s+)(\w+)$").unwrap();
            let result = re.replace_all(content, "  $2: true");
            format!(
                "{result}\n## OpenCode Sub-agent\n\nThis agent can be invoked as an OpenCode sub-agent.\n"
            )
        }
        _ => content.to_string(),
    }
}

// ── Install / Uninstall ──────────────────────────────────────────────────

pub fn install_tool(tool_id: &str, components: Option<&[String]>) {
    let cfg = match tool_config(tool_id) {
        Some(c) => c,
        None => {
            eprintln!("Unknown tool: {tool_id}");
            eprintln!(
                "Available: {}",
                ALL_TOOLS
                    .iter()
                    .map(|(id, _)| *id)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            return;
        }
    };

    let want_mcp = components.is_none() || components.unwrap().iter().any(|c| c == "mcp");
    let want_skills = components.is_none() || components.unwrap().iter().any(|c| c == "skills");
    let want_agents = components.is_none() || components.unwrap().iter().any(|c| c == "agents");

    println!("\nInstalling Obscura for {}...\n", cfg.name);

    if want_mcp && cfg.supports_mcp {
        println!("[MCP Server]");
        inject_mcp(&cfg);
    }

    if want_skills && cfg.supports_skills {
        println!("\n[Skills]");
        if tool_id == "cursor" {
            let mut combined = String::from(
                "---\ndescription: Obscura browser automation skills\nglobs:\nalwaysApply: true\n---\n\n# Obscura Browser Skills\n\n",
            );
            for (name, content) in CANONICAL_SKILLS {
                let transformed = transform_skill(content, tool_id);
                combined.push_str(&format!("## {name}\n\n{transformed}\n\n---\n\n"));
            }
            let dest = (cfg.skills_dir)("");
            upsert_file(&dest, &combined);
        } else {
            for (name, content) in CANONICAL_SKILLS {
                let transformed = transform_skill(content, tool_id);
                let dest = (cfg.skills_dir)(name);
                upsert_file(&dest, &transformed);
            }
        }
    }

    if want_agents && cfg.supports_agents {
        println!("\n[Agents]");
        for (name, content) in CANONICAL_AGENTS {
            let transformed = transform_agent(content, tool_id);
            let dest = (cfg.agents_dir)(name);
            upsert_file(&dest, &transformed);
            // For tools that nest agents inside skill dirs (e.g. Codex),
            // also seed the agent SKILL.md alongside the yaml.
            if tool_id == "codex" {
                let skill_dest = home().join(format!(".codex/skills/{name}/SKILL.md"));
                upsert_file(&skill_dest, content);
            }
        }
    }

    println!("\nDone! Obscura installed for {}.\n", cfg.name);
}

pub fn uninstall_tool(tool_id: &str) {
    let cfg = match tool_config(tool_id) {
        Some(c) => c,
        None => {
            eprintln!("Unknown tool: {tool_id}");
            return;
        }
    };

    println!("\nUninstalling Obscura from {}...\n", cfg.name);

    if cfg.supports_mcp {
        println!("[MCP Server]");
        remove_mcp(&cfg);
    }

    if cfg.supports_skills {
        println!("\n[Skills]");
        if tool_id == "cursor" {
            let dest = (cfg.skills_dir)("");
            if dest.exists() {
                let _ = fs::remove_file(&dest);
                println!("  removed: {}", dest.display());
            }
        } else {
            for (name, _) in CANONICAL_SKILLS {
                let dest = (cfg.skills_dir)(name);
                if dest.exists() {
                    let _ = fs::remove_file(&dest);
                    println!("  removed: {}", dest.display());
                    // remove parent dir if empty (e.g. ~/.claude/skills/obscura-fetch/)
                    if let Some(parent) = dest.parent() {
                        let _ = fs::remove_dir(parent);
                    }
                }
            }
        }
    }

    if cfg.supports_agents {
        println!("\n[Agents]");
        for (name, _) in CANONICAL_AGENTS {
            let dest = (cfg.agents_dir)(name);
            if dest.exists() {
                let _ = fs::remove_file(&dest);
                println!("  removed: {}", dest.display());
            }
        }
    }

    println!("\nDone! Obscura uninstalled from {}.\n", cfg.name);
}

// ── List ─────────────────────────────────────────────────────────────────

pub fn list_tools() {
    println!("\nObscura MCP — Install Targets\n");
    println!(
        "  {:<9} {:<15} {:<5} {:<6} Agents",
        "ID", "Tool", "MCP", "Skills"
    );
    println!(
        "  {:<9} {:<15} {:<5} {:<6} ─────",
        "─────", "─────", "─────", "─────"
    );
    for (id, _) in ALL_TOOLS {
        if let Some(cfg) = tool_config(id) {
            println!(
                "  {id:<9} {:<15} {:<5} {:<6} {}",
                cfg.name,
                if cfg.supports_mcp { "yes" } else { "no" },
                if cfg.supports_skills { "yes" } else { "no" },
                if cfg.supports_agents { "yes" } else { "no" },
            );
        }
    }
    println!();
}
