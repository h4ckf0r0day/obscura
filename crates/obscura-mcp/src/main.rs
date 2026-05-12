use clap::{Parser, Subcommand};

use obscura_mcp::{install, mcp, wizard};

#[derive(Parser)]
#[command(
    name = "obscura-mcp",
    about = "MCP server for Obscura headless browser"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run MCP server (stdio JSON-RPC)
    Serve,

    /// Install integrations to AI coding tools
    Install {
        /// Tool: claude, cursor, gemini, codex, opencode, cline, all
        tool: Option<String>,
        /// Components: mcp, skills, agents
        components: Vec<String>,
    },

    /// Uninstall integrations from AI coding tools
    Uninstall {
        /// Tool: claude, cursor, gemini, codex, opencode, cline, all
        tool: String,
    },

    /// List supported tools
    List,
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Serve => mcp::run(),
        Commands::Install { tool, components } => {
            let comps = if components.is_empty() {
                None
            } else {
                Some(components)
            };

            match tool {
                None => {
                    let selected = wizard::interactive_select();
                    if selected.is_empty() {
                        println!("\n  Cancelled.\n");
                    } else {
                        for t in &selected {
                            install::install_tool(t, comps.as_deref());
                        }
                    }
                }
                Some(t) if t == "all" => {
                    for (id, _) in install::ALL_TOOLS {
                        install::install_tool(id, comps.as_deref());
                    }
                }
                Some(t) => {
                    for t in t.split(',') {
                        install::install_tool(t.trim(), comps.as_deref());
                    }
                }
            }
        }
        Commands::Uninstall { tool } => {
            if tool == "all" {
                for (id, _) in install::ALL_TOOLS {
                    install::uninstall_tool(id);
                }
            } else {
                for t in tool.split(',') {
                    install::uninstall_tool(t.trim());
                }
            }
        }
        Commands::List => install::list_tools(),
    }
}
