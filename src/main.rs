mod cli;
mod commands;
mod mcp;

// Re-export lib modules as crate-level so commands/cli/mcp can use crate::db, etc.
pub use cartog::db;
pub use cartog::indexer;
pub use cartog::languages;
pub use cartog::rag;
pub use cartog::types;
pub use cartog::watch;

use anyhow::Result;
use clap::Parser;

use cli::{Cli, Command, RagCommand};

fn main() -> Result<()> {
    let cli = Cli::parse();

    let is_serve = matches!(cli.command, Command::Serve { .. });
    let is_watch = matches!(cli.command, Command::Watch { .. });
    let is_rag = matches!(
        cli.command,
        Command::Rag(RagCommand::Index { .. }) | Command::Rag(RagCommand::Setup)
    );
    let default_level = if is_serve || is_rag || is_watch {
        "info"
    } else {
        "warn"
    };

    // Initialize tracing to stderr for all commands.
    // - CLI mode: only warnings (e.g., unparseable files) show by default
    // - Serve / RAG index / Watch mode: info-level for progress
    // Stdout stays clean for CLI output and MCP protocol.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_level)),
        )
        .init();

    match cli.command {
        Command::Index { path, force } => commands::cmd_index(&path, force, cli.json),
        Command::Outline { file } => commands::cmd_outline(&file, cli.json),
        Command::Callees { name } => commands::cmd_callees(&name, cli.json),
        Command::Impact { name, depth } => commands::cmd_impact(&name, depth, cli.json),
        Command::Refs { name, kind } => commands::cmd_refs(&name, kind, cli.json),
        Command::Hierarchy { name } => commands::cmd_hierarchy(&name, cli.json),
        Command::Deps { file } => commands::cmd_deps(&file, cli.json),
        Command::Stats => commands::cmd_stats(cli.json),
        Command::Search {
            query,
            kind,
            file,
            limit,
        } => commands::cmd_search(&query, kind, file.as_deref(), limit, cli.json),
        Command::Watch {
            path,
            debounce,
            rag,
            rag_delay,
        } => commands::cmd_watch(&path, debounce, rag, rag_delay),
        Command::Serve { watch, rag } => {
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(mcp::run_server(watch, rag))
        }
        Command::Rag(rag_cmd) => match rag_cmd {
            RagCommand::Setup => commands::cmd_rag_setup(cli.json),
            RagCommand::Index { path, force } => commands::cmd_rag_index(&path, force, cli.json),
            RagCommand::Search { query, kind, limit } => {
                commands::cmd_rag_search(&query, kind, limit, cli.json)
            }
        },
    }
}
