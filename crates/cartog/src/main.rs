mod cli;
mod commands;
mod config;
use cartog::auto_check::{self, CommandKind, MaybeSpawnInput};
use cartog::state;

use anyhow::Result;
use cartog_mcp as mcp;
use clap::Parser;
use std::io::IsTerminal;
use std::path::Path;
use std::time::SystemTime;

use cli::{Cli, Command, RagCommand, SelfCommand};

/// Public-default GitHub latest-release endpoint for the daily background
/// check. Override via `CARTOG_GITHUB_API_URL` (used by integration tests).
const DEFAULT_GITHUB_LATEST_URL: &str =
    "https://api.github.com/repos/jrollin/cartog/releases/latest";

/// Long-lived commands (`serve`, `watch`) skip the auto-check — they run
/// for hours and the user never sees a hint printed at the *start* anyway.
fn classify_command(cmd: &Command) -> CommandKind {
    match cmd {
        Command::Serve { .. } | Command::Watch { .. } => CommandKind::LongLived,
        _ => CommandKind::Quick,
    }
}

/// Walk up from cwd to a `.git` directory; fall back to cwd. Used by
/// `migrate-db` when the resolved DB path is outside the project.
fn project_root_from_cwd() -> std::path::PathBuf {
    use std::path::PathBuf;
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut dir = cwd.clone();
    loop {
        if dir.join(".git").exists() {
            return dir;
        }
        if !dir.pop() {
            break;
        }
    }
    cwd
}

fn run_auto_check_epilogue(command_kind: CommandKind) {
    let api_url = std::env::var("CARTOG_GITHUB_API_URL")
        .unwrap_or_else(|_| DEFAULT_GITHUB_LATEST_URL.to_string());
    let state_path = state::default_state_file();
    let disabled_env = std::env::var("CARTOG_NO_UPDATE_CHECK").ok();
    let mode_env = std::env::var("CARTOG_UPDATE_CHECK").ok();
    let stdout_is_tty = std::io::stdout().is_terminal();

    auto_check::maybe_spawn(MaybeSpawnInput {
        command_kind,
        stdout_is_tty,
        disabled_env: disabled_env.as_deref(),
        mode_env: mode_env.as_deref(),
        state_path: state_path.as_deref(),
        api_url: &api_url,
        current_version: env!("CARGO_PKG_VERSION"),
        now: SystemTime::now(),
    });
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Resolve database path: --db / CARTOG_DB > .cartog.toml > git root > cwd
    let (cartog_config, config_path) = config::load_config();
    let db_path = config::resolve_db_path(cli.db.clone(), &cartog_config);
    let provider_config = config::to_provider_config(&cartog_config);
    let embedding_dim = provider_config.resolved_dimension();
    let search_tuning = cartog_config
        .rag
        .as_ref()
        .map(|r| r.to_search_tuning())
        .unwrap_or_default();

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

    // Surface resolved paths once tracing is live so `-v` / RUST_LOG=info users
    // can see which config and DB are actually in effect.
    if let Some(ref p) = config_path {
        tracing::info!(path = %p.display(), "loaded .cartog.toml");
    } else {
        tracing::debug!("no .cartog.toml found; using defaults");
    }
    tracing::debug!(path = %db_path.display(), "resolved database path");

    // Token budget only applies to human-readable output
    let token_budget = if cli.json { None } else { cli.tokens };

    // Classify before the match consumes cli.command.
    let command_kind = classify_command(&cli.command);

    let result = match cli.command {
        Command::Index {
            path,
            force,
            no_lsp,
        } => commands::cmd_index(&db_path, &path, force, !no_lsp, cli.json, embedding_dim),
        Command::Outline { file } => {
            commands::cmd_outline(&db_path, &file, cli.json, token_budget, embedding_dim)
        }
        Command::Callees { name } => {
            commands::cmd_callees(&db_path, &name, cli.json, token_budget, embedding_dim)
        }
        Command::Impact { name, depth } => commands::cmd_impact(
            &db_path,
            &name,
            depth,
            cli.json,
            token_budget,
            embedding_dim,
        ),
        Command::Refs { name, kind } => {
            commands::cmd_refs(&db_path, &name, kind, cli.json, token_budget, embedding_dim)
        }
        Command::Hierarchy { name } => {
            commands::cmd_hierarchy(&db_path, &name, cli.json, token_budget, embedding_dim)
        }
        Command::Deps { file } => {
            commands::cmd_deps(&db_path, &file, cli.json, token_budget, embedding_dim)
        }
        Command::Stats => commands::cmd_stats(&db_path, cli.json, embedding_dim),
        Command::Config => {
            commands::cmd_config(&cartog_config, config_path.as_deref(), &db_path, cli.json)
        }
        Command::Doctor => commands::cmd_doctor(
            &cartog_config,
            config_path.as_deref(),
            &db_path,
            cli.json,
            embedding_dim,
            &provider_config,
        ),
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
            embedding_dim,
        ),
        Command::Map { tokens } => commands::cmd_map(&db_path, tokens, cli.json, embedding_dim),
        Command::Changes { commits, kind } => commands::cmd_changes(
            &db_path,
            commits,
            kind,
            cli.json,
            token_budget,
            embedding_dim,
        ),
        Command::Watch {
            path,
            debounce,
            rag,
            rag_delay,
        } => commands::cmd_watch(
            &db_path,
            &path,
            debounce,
            rag,
            rag_delay,
            provider_config,
            cli.json,
        ),
        Command::Init { dry_run } => commands::init::cmd_init(dry_run, cli.json),
        Command::Ide {
            client,
            scope,
            yes,
            dry_run,
            no_watch,
        } => commands::ide::cmd_ide(client, scope, yes, dry_run, no_watch, cli.json),
        Command::Serve { watch, rag } => {
            let runtime = tokio::runtime::Runtime::new()?;
            let opts = mcp::ServerOptions {
                pid_lock_dir: state::default_state_dir(),
            };
            runtime.block_on(mcp::run_server(&db_path, watch, rag, provider_config, opts))
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
                &search_tuning,
            ),
        },
        Command::Completions { shell } => {
            use clap::CommandFactory;
            let mut cmd = Cli::command();
            clap_complete::generate(shell, &mut cmd, "cartog", &mut std::io::stdout());
            Ok(())
        }
        Command::Manpage => {
            use clap::CommandFactory;
            let cmd = Cli::command();
            clap_mangen::Man::new(cmd)
                .render(&mut std::io::stdout())
                .map_err(Into::into)
        }
        Command::Self_(sub) => match sub {
            SelfCommand::Update { check, quiet } => {
                commands::cmd_self_update(check, quiet, cli.json)
            }
            SelfCommand::Version => commands::cmd_self_version(cli.json),
            SelfCommand::Rollback => commands::cmd_self_rollback(),
            SelfCommand::MigrateDb { dry_run } => {
                // Explicit --db/CARTOG_DB/[database].path can point outside the project;
                // anchor at the project root in that case, not at the DB parent.
                let explicit_override = cli.db.is_some()
                    || cartog_config
                        .database
                        .as_ref()
                        .is_some_and(|d| d.path.is_some());
                let root = if explicit_override {
                    project_root_from_cwd()
                } else {
                    let parent = db_path
                        .parent()
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));
                    if parent.file_name().and_then(|n| n.to_str()) == Some(cartog_db::DB_DIR) {
                        parent.parent().map(Path::to_path_buf).unwrap_or(parent)
                    } else {
                        parent
                    }
                };
                commands::cmd_self_migrate_db(&root, dry_run, cli.json)
            }
        },
    };

    run_auto_check_epilogue(command_kind);

    result
}
