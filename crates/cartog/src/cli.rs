use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

use cartog_core::{EdgeKind, SymbolKind};

/// Extended version string printed by `cartog --version` (long form).
/// Short form (`-V`) keeps the bare semver. Populated by `build.rs`.
pub const LONG_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    "\ndescribe: ",
    env!("CARTOG_BUILD_VERSION"),
    "\nbuild:    ",
    env!("CARTOG_BUILD_SHA"),
    "\nfeatures: ",
    env!("CARTOG_BUILD_FEATURES"),
    "\nrustc:    ",
    env!("CARGO_PKG_RUST_VERSION"),
    " (MSRV)",
);

#[derive(Debug, Parser)]
#[command(name = "cartog")]
#[command(about = "Map your codebase. Navigate by graph, not grep.")]
#[command(version)]
#[command(long_version = LONG_VERSION)]
#[command(propagate_version = true)]
#[command(after_help = "Docs: https://www.cartog.dev/")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    /// Output as JSON
    #[arg(long, global = true)]
    pub json: bool,

    /// Limit human-readable output to approximately N tokens (ignored with --json)
    #[arg(long, global = true)]
    pub tokens: Option<u32>,

    /// Drop heavy fields (bodies, docstrings, cache hashes) from --json output to
    /// save agent tokens. No-op without --json.
    #[arg(long, global = true)]
    pub compact: bool,

    /// Path to the cartog database (overrides .cartog.toml and auto-detection)
    #[arg(long, global = true, value_name = "PATH", env = "CARTOG_DB")]
    pub db: Option<PathBuf>,
}

/// Filter for symbol kinds in the search command.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum SymbolKindFilter {
    Function,
    Class,
    Method,
    Variable,
    Import,
    Interface,
    Enum,
    EnumMember,
    TypeAlias,
    Trait,
    Module,
    Document,
    Macro,
    Component,
    /// Include all symbol kinds (code + documents).
    All,
}

impl From<SymbolKindFilter> for SymbolKind {
    fn from(f: SymbolKindFilter) -> Self {
        match f {
            SymbolKindFilter::Function => SymbolKind::Function,
            SymbolKindFilter::Class => SymbolKind::Class,
            SymbolKindFilter::Method => SymbolKind::Method,
            SymbolKindFilter::Variable => SymbolKind::Variable,
            SymbolKindFilter::Import => SymbolKind::Import,
            SymbolKindFilter::Interface => SymbolKind::Interface,
            SymbolKindFilter::Enum => SymbolKind::Enum,
            SymbolKindFilter::EnumMember => SymbolKind::EnumMember,
            SymbolKindFilter::TypeAlias => SymbolKind::TypeAlias,
            SymbolKindFilter::Trait => SymbolKind::Trait,
            SymbolKindFilter::Module => SymbolKind::Module,
            SymbolKindFilter::Document => SymbolKind::Document,
            SymbolKindFilter::Macro => SymbolKind::Macro,
            SymbolKindFilter::Component => SymbolKind::Component,
            SymbolKindFilter::All => unreachable!("All is not a single SymbolKind"),
        }
    }
}

/// Filter for edge kinds in the refs command.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum EdgeKindFilter {
    Calls,
    Imports,
    Inherits,
    References,
    Raises,
    Implements,
    TypeOf,
}

impl From<EdgeKindFilter> for EdgeKind {
    fn from(f: EdgeKindFilter) -> Self {
        match f {
            EdgeKindFilter::Calls => EdgeKind::Calls,
            EdgeKindFilter::Imports => EdgeKind::Imports,
            EdgeKindFilter::Inherits => EdgeKind::Inherits,
            EdgeKindFilter::References => EdgeKind::References,
            EdgeKindFilter::Raises => EdgeKind::Raises,
            EdgeKindFilter::Implements => EdgeKind::Implements,
            EdgeKindFilter::TypeOf => EdgeKind::TypeOf,
        }
    }
}

/// MCP client targeted by `cartog ide`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClientKind {
    Antigravity,
    ClaudeCode,
    ClaudeDesktop,
    Codex,
    Cursor,
    Gemini,
    Hermes,
    Kiro,
    Opencode,
    Vscode,
    Windsurf,
    Zed,
}

/// Scope filter for `cartog ide`: project-scoped configs, user-scoped configs, or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, serde::Serialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum IdeScope {
    Project,
    User,
    #[default]
    All,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Build or rebuild the code graph index
    Index {
        /// Directory to index (defaults to current directory)
        #[arg(default_value = ".")]
        path: String,

        /// Force full re-index, bypassing change detection
        #[arg(long)]
        force: bool,

        /// Disable LSP-based edge resolution (auto-detected by default when servers are on PATH)
        #[arg(long)]
        no_lsp: bool,

        /// Parse worker threads. 0 or omitted = auto (CPU count); clamped
        /// 1..=64. Overrides CARTOG_JOBS and [index] jobs.
        #[arg(long, value_name = "N")]
        jobs: Option<usize>,
    },

    /// Show symbols and structure of a file
    Outline {
        /// File path to outline
        file: String,
    },

    /// Find what a symbol calls
    Callees {
        /// Symbol name to search for
        name: String,
    },

    /// Transitive impact analysis — what breaks if this changes?
    Impact {
        /// Symbol name to analyze
        name: String,

        /// Maximum depth of transitive analysis
        #[arg(long, default_value = "3")]
        depth: u32,
    },

    /// Build a one-shot task-context bundle: relevant symbols + bodies for a task
    Context {
        /// Natural-language description of the task
        task: String,

        /// Approximate token budget for the bundle
        #[arg(long, default_value = "6000")]
        tokens: u32,
    },

    /// Find a call path between two symbols, with each hop's body inline
    Trace {
        /// Starting symbol (the caller end of the path)
        from: String,

        /// Target symbol (the callee end of the path)
        to: String,

        /// Maximum path length to search
        #[arg(long, default_value = "8")]
        depth: u32,
    },

    /// All references to a symbol (calls, imports, inherits, references, raises)
    Refs {
        /// Symbol name to search for
        name: String,

        /// Filter by edge kind
        #[arg(long)]
        kind: Option<EdgeKindFilter>,
    },

    /// Show inheritance hierarchy for a class
    Hierarchy {
        /// Class name
        name: String,

        /// Render the hierarchy as a Mermaid `graph TD` diagram instead of plain text
        ///
        /// Paste the output into any Mermaid renderer (GitHub, mermaid.live, ...).
        /// Ignored when `--json` is also set.
        #[arg(long)]
        mermaid: bool,
    },

    /// File-level import dependencies
    Deps {
        /// File path
        file: String,

        /// Render the imports as a Mermaid `graph LR` diagram instead of plain text
        ///
        /// Ignored when `--json` is also set.
        #[arg(long)]
        mermaid: bool,
    },

    /// Index statistics summary
    Stats {
        /// Show per-tool query counts and estimated tokens saved vs grep+read
        ///
        /// Reads the local query log; no network calls.
        #[arg(long)]
        savings: bool,
    },

    /// Per-tool query counts + estimated tokens saved.
    ///
    /// Alias for `cartog stats --savings`, promoted to a top-level verb so
    /// day-to-day savings are one keystroke away.
    Savings,

    /// Upload the local index to an S3-compatible remote (opt-in feature).
    ///
    /// Reads `[remote].url` from `.cartog.toml` unless `--remote` is given.
    /// Credentials come from the AWS environment chain (env vars / profile /
    /// IMDS); cartog never reads credentials from `.cartog.toml`.
    Push {
        /// Override `s3://bucket/key` target.
        #[arg(long)]
        remote: Option<String>,
    },

    /// Download an index from an S3-compatible remote (opt-in feature).
    ///
    /// Refuses to overwrite the local DB while a peer (`cartog serve` /
    /// `cartog watch`) holds it open, unless `--force` is given. Verifies a
    /// SHA-256 checksum and schema version before atomic rename.
    Pull {
        /// Override `s3://bucket/key` target.
        #[arg(long)]
        remote: Option<String>,

        /// Overwrite the local DB even if a peer process is currently using it.
        #[arg(long)]
        force: bool,

        /// Skip credential resolution and pull anonymously (public buckets).
        #[arg(long)]
        no_sign_request: bool,
    },

    /// Display the current configuration
    Config,

    /// Check that requirements are met and everything is working
    Doctor,

    /// Search symbols by name (case-insensitive prefix + substring match)
    Search {
        /// Query string to match against symbol names
        query: String,

        /// Filter by symbol kind
        #[arg(long)]
        kind: Option<SymbolKindFilter>,

        /// Filter to a specific file path
        #[arg(long)]
        file: Option<String>,

        /// Maximum results to return (capped at 100)
        #[arg(long, default_value = "30")]
        limit: u32,
    },

    /// Token-budget-aware codebase summary (file tree + top symbols by centrality)
    Map {
        /// Approximate token budget for the output
        #[arg(long, default_value = "4000")]
        tokens: u32,

        /// Render the file tree as a Mermaid `graph TD` diagram instead of indented text
        ///
        /// Token budget still applies. Ignored when `--json` is also set.
        #[arg(long)]
        mermaid: bool,
    },

    /// Show symbols affected by recent git changes
    Changes {
        /// Number of recent commits to consider
        #[arg(long, default_value = "5")]
        commits: u32,

        /// Filter by symbol kind
        #[arg(long)]
        kind: Option<SymbolKindFilter>,
    },

    /// Watch for file changes and auto-re-index
    Watch {
        /// Directory to watch (defaults to current directory)
        #[arg(default_value = ".")]
        path: String,

        /// Debounce window in seconds
        #[arg(long, default_value = "5")]
        debounce: u64,

        /// Turn on RAG embedding when auto-detection would leave it off
        ///
        /// Default is automatic: on when the repo already has embeddings.
        /// `[embedding] auto_embed` (config) and CARTOG_WATCH_RAG (env) take
        /// precedence over this flag.
        #[arg(long)]
        rag: bool,

        /// Delay in seconds before batch embedding after last index
        #[arg(long, default_value = "30")]
        rag_delay: u64,
    },

    /// Bootstrap cartog config in the current project: scaffold a `.cartog.toml` template.
    ///
    /// Run `cartog ide` afterwards to wire editor MCP entries, and `cartog index`
    /// to build the code graph. Each verb does one job: edit the toml between
    /// steps to change DB path or embedding provider before any heavy work runs.
    Init {
        /// Print planned changes without writing.
        #[arg(long)]
        dry_run: bool,
    },

    /// Wire `cartog serve` into one or all MCP-compatible editors.
    ///
    /// See `--client` for the supported editors. User-scope clients whose
    /// config directory does not exist are skipped (not installed).
    ///
    /// See also `cartog install <client>...` for a positional shorthand that
    /// matches the brew/npm/pip convention.
    Ide {
        /// Target a single client. Default: configure all clients in scope.
        #[arg(long, value_enum)]
        client: Option<ClientKind>,

        /// Filter by scope
        ///
        /// `project` writes only .mcp.json / .cursor/mcp.json; `user` writes
        /// only user-scope configs; `all` writes both.
        #[arg(long, value_enum, default_value_t = IdeScope::All)]
        scope: IdeScope,

        /// Accept all prompts (non-interactive)
        ///
        /// Implied by --dry-run, --json, --client, or a non-TTY stdin.
        #[arg(long, short = 'y')]
        yes: bool,

        /// Print planned changes without writing.
        #[arg(long)]
        dry_run: bool,

        /// Omit `--watch` from every client's serve args (default wires `--watch` for all).
        #[arg(long)]
        no_watch: bool,
    },

    /// Install cartog MCP config into one or more editors.
    ///
    /// Friendlier shape of `cartog ide`: takes editors as positional arguments
    /// (`cartog install cursor`, `cartog install cursor zed codex`) so it
    /// matches the brew/npm/pip/cargo convention. No positional clients =
    /// install into every detected editor non-interactively.
    ///
    /// Positional mode is always non-interactive (`--yes` implied). For the
    /// interactive picker, use `cartog ide` directly.
    Install {
        /// One or more editors to wire up
        ///
        /// Omit to install into every detected editor non-interactively.
        clients: Vec<ClientKind>,

        /// Filter by scope
        ///
        /// `project` writes only .mcp.json / .cursor/mcp.json; `user` writes
        /// only user-scope configs; `all` writes both.
        #[arg(long, value_enum, default_value_t = IdeScope::All)]
        scope: IdeScope,

        /// Print planned changes without writing.
        #[arg(long)]
        dry_run: bool,

        /// Omit `--watch` from every client's serve args (default wires `--watch` for all).
        #[arg(long)]
        no_watch: bool,
    },

    /// Start MCP server over stdio (for Claude Code, Cursor, and other MCP clients)
    Serve {
        /// Enable file watching with auto-re-index during MCP session
        #[arg(long)]
        watch: bool,

        /// Turn on RAG embedding when auto-detection would leave it off
        ///
        /// Default is automatic: on when the repo already has embeddings.
        /// `[embedding] auto_embed` (config) and CARTOG_WATCH_RAG (env) take
        /// precedence over this flag. Only meaningful with `--watch` (warns
        /// otherwise).
        #[arg(long)]
        rag: bool,
    },

    /// Semantic code search (RAG pipeline)
    #[command(subcommand)]
    Rag(RagCommand),

    /// Inspect the machine-local registry of indexed cartog projects
    #[command(subcommand)]
    Projects(ProjectsCommand),

    /// Manage the cartog installation: upgrade, inspect, roll back
    #[command(name = "self", subcommand)]
    Self_(SelfCommand),

    /// Generate shell completions for bash, zsh, fish, elvish, or powershell.
    ///
    /// Example: `cartog completions bash > ~/.local/share/bash-completion/completions/cartog`
    Completions {
        /// Shell to generate completions for
        shell: clap_complete::Shell,
    },

    /// Emit a troff-formatted manpage for `cartog` on stdout.
    ///
    /// Example: `cartog manpage > cartog.1 && man ./cartog.1`
    Manpage,
}

#[derive(Debug, Subcommand)]
pub enum ProjectsCommand {
    /// List every indexed project on this machine, with staleness markers
    List,

    /// Register an already-indexed project without re-indexing it.
    ///
    /// Use this to make a project cartog indexed a while ago show up in
    /// `projects list` again. It refuses when there is no index at the path —
    /// it registers an existing one, it never creates one.
    Add {
        /// Project root to register (defaults to the current directory)
        #[arg(default_value = ".")]
        path: String,
    },

    /// Register every already-indexed project under a directory
    ///
    /// Walks only the directory you name — never `$HOME` by default.
    Scan {
        /// Directory to search for indexed projects
        dir: String,

        /// How many levels below `dir` to search
        #[arg(long, default_value = "2")]
        depth: u32,

        /// Report what would be registered without writing anything
        #[arg(long)]
        dry_run: bool,
    },

    /// Drop one project's registry row. Never touches its index.
    Forget {
        /// Project id, root path, database path, or name — either the declared
        /// `[project] name` or the directory basename (from `projects list`)
        target: String,
    },

    /// Drop registry rows whose database file no longer exists
    Prune {
        /// Report what would be dropped without changing the registry
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum RagCommand {
    /// Download embedding + re-ranker models from HuggingFace
    Setup,

    /// Build embedding index for semantic search (requires setup first)
    Index {
        /// Directory to index (defaults to current directory)
        #[arg(default_value = ".")]
        path: String,

        /// Force re-embed all symbols
        #[arg(long)]
        force: bool,
    },

    /// Semantic search over code symbols
    Search {
        /// Natural language query
        query: String,

        /// Filter by symbol kind
        #[arg(long)]
        kind: Option<SymbolKindFilter>,

        /// Maximum results to return
        #[arg(long, default_value = "10")]
        limit: u32,
    },
}

#[derive(Debug, Subcommand)]
pub enum SelfCommand {
    /// Upgrade cartog in place (or check, defer, or apply a deferred update)
    Update {
        /// Report whether an update is available without modifying anything
        ///
        /// Exit codes: 0 up to date, 1 update available, 2 network/parse error.
        #[arg(long, conflicts_with_all = ["defer", "apply_pending"])]
        check: bool,

        /// Arm a deferred update without swapping the binary
        ///
        /// Records the target version in the state file and exits. Succeeds
        /// even while a peer `cartog serve`/`watch` is running — the swap
        /// happens later via `--apply-pending` once the peer has exited. This
        /// is the right call from inside a Claude Code session, where the MCP
        /// server is the peer. Targets the latest stable release unless `--to`
        /// pins a version.
        #[arg(long, conflicts_with_all = ["check", "apply_pending"])]
        defer: bool,

        /// With `--defer`, arm exactly this `MAJOR.MINOR.PATCH` version
        ///
        /// Overrides resolving the latest stable release. Used by
        /// `/cartog-install` to arm the plugin's pinned version. Requires
        /// `--defer`.
        #[arg(long, value_name = "VERSION", requires = "defer")]
        to: Option<String>,

        /// Apply a previously-armed deferred update (see `--defer`)
        ///
        /// Reads the pending target from the state file, waits briefly for any
        /// peer lock to clear, performs the swap, and clears the pending
        /// intent. Intended to run from the SessionEnd hook once the serve
        /// process has exited.
        #[arg(long, conflicts_with_all = ["check", "defer"])]
        apply_pending: bool,

        /// With `--apply-pending`, exclude THIS project's own serve/watch peer
        /// from the peer-wait.
        ///
        /// At SessionStart the session's own `cartog serve --watch` has just
        /// taken the serve lock and will hold it all session, so a normal
        /// peer-wait can never clear and the swap never lands. The atomic
        /// same-FS swap is safe under a live same-project peer (it keeps its fd
        /// on the old inode until it re-execs), so we ignore only that peer.
        /// Other projects' peers still block. No-op on Windows (a running .exe
        /// cannot be renamed while a peer holds it). Requires `--apply-pending`.
        #[arg(long, requires = "apply_pending")]
        at_startup: bool,

        /// Suppress all output; the exit code is the sole signal.
        #[arg(long)]
        quiet: bool,
    },

    /// Show installed version, target triple, install source, and last check time
    Version,

    /// Restore the previous binary saved at `<bin>.old`
    Rollback,

    /// Move a legacy `.cartog.db` (+ WAL/SHM/backups) into `.cartog/db.sqlite`
    ///
    /// Detects the project root via the same rules as the rest of cartog
    /// (walk up to the git root, or use cwd). Refuses to run while another
    /// cartog process holds the peer lock, and never overwrites files at
    /// the destination.
    #[command(name = "migrate-db")]
    MigrateDb {
        /// Print the planned moves without touching the filesystem.
        #[arg(long)]
        dry_run: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::ValueEnum;

    /// `SymbolKindFilter` mirrors `SymbolKind` by hand, so a variant added to core
    /// compiles fine here while silently missing from `--kind`. Comparing wire
    /// strings catches the drift.
    #[test]
    fn kind_filter_covers_every_core_symbol_kind() {
        let exposed: std::collections::HashSet<String> = SymbolKindFilter::value_variants()
            .iter()
            .filter(|f| !matches!(f, SymbolKindFilter::All))
            .map(|f| SymbolKind::from(*f).as_str().to_string())
            .collect();

        for kind in cartog_core::ALL_SYMBOL_KINDS {
            assert!(
                exposed.contains(kind.as_str()),
                "SymbolKind::{kind:?} is missing from --kind (add it to SymbolKindFilter)"
            );
        }
    }

    /// `All` must stay filtered out before `From` runs, or the conversion panics.
    #[test]
    fn every_non_all_filter_converts_without_panicking() {
        for f in SymbolKindFilter::value_variants() {
            if !matches!(f, SymbolKindFilter::All) {
                let _ = SymbolKind::from(*f);
            }
        }
    }
}
