mod cli;
mod commands;
mod db;
mod indexer;
mod languages;
mod mcp;
mod types;

use anyhow::Result;
use clap::Parser;

use cli::{Cli, Command};

fn main() -> Result<()> {
    let cli = Cli::parse();

    let is_serve = matches!(cli.command, Command::Serve);
    let default_level = if is_serve { "info" } else { "warn" };

    // Initialize tracing to stderr for all commands.
    // - CLI mode: only warnings (e.g., unparseable files) show by default
    // - Serve mode: info-level lifecycle events + debug per-request with RUST_LOG=debug
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
        Command::Serve => {
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(mcp::run_server())
        }
    }
}
