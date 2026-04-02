mod cli;
mod commands;
mod config;

use anyhow::Result;
use cartog_mcp as mcp;
use clap::Parser;

use cli::{Cli, Command, RagCommand};

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Resolve database path: --db / CARTOG_DB > .cartog.toml > git root > cwd
    let (cartog_config, config_path) = config::load_config();
    let db_path = config::resolve_db_path(cli.db.clone(), &cartog_config);
    let provider_config = config::to_provider_config(&cartog_config);

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

    // Token budget only applies to human-readable output
    let token_budget = if cli.json { None } else { cli.tokens };

    match cli.command {
        Command::Index {
            path,
            force,
            no_lsp,
        } => commands::cmd_index(&db_path, &path, force, !no_lsp, cli.json),
        Command::Outline { file } => commands::cmd_outline(&db_path, &file, cli.json, token_budget),
        Command::Callees { name } => commands::cmd_callees(&db_path, &name, cli.json, token_budget),
        Command::Impact { name, depth } => {
            commands::cmd_impact(&db_path, &name, depth, cli.json, token_budget)
        }
        Command::Refs { name, kind } => {
            commands::cmd_refs(&db_path, &name, kind, cli.json, token_budget)
        }
        Command::Hierarchy { name } => {
            commands::cmd_hierarchy(&db_path, &name, cli.json, token_budget)
        }
        Command::Deps { file } => commands::cmd_deps(&db_path, &file, cli.json, token_budget),
        Command::Stats => commands::cmd_stats(&db_path, cli.json),
        Command::Config => {
            commands::cmd_config(&cartog_config, config_path.as_deref(), &db_path, cli.json)
        }
        Command::Search {
            query,
            kind,
            file,
            limit,
        } => commands::cmd_search(
            &db_path,
            &query,
            kind,
            file.as_deref(),
            limit,
            cli.json,
            token_budget,
        ),
        Command::Map { tokens } => commands::cmd_map(&db_path, tokens, cli.json),
        Command::Changes { commits, kind } => {
            commands::cmd_changes(&db_path, commits, kind, cli.json, token_budget)
        }
        Command::Watch {
            path,
            debounce,
            rag,
            rag_delay,
        } => commands::cmd_watch(&db_path, &path, debounce, rag, rag_delay, provider_config),
        Command::Serve { watch, rag } => {
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(mcp::run_server(&db_path, watch, rag, provider_config))
        }
        Command::Rag(rag_cmd) => match rag_cmd {
            RagCommand::Setup => commands::cmd_rag_setup(cli.json),
            RagCommand::Index { path, force } => {
                commands::cmd_rag_index(&db_path, &path, force, cli.json, &provider_config)
            }
            RagCommand::Search { query, kind, limit } => commands::cmd_rag_search(
                &db_path,
                &query,
                kind,
                limit,
                cli.json,
                token_budget,
                &provider_config,
            ),
        },
    }
}
