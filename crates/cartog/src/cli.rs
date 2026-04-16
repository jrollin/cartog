use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

use cartog_core::{EdgeKind, SymbolKind};

#[derive(Debug, Parser)]
#[command(name = "cartog")]
#[command(about = "Map your codebase. Navigate by graph, not grep.")]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    /// Output as JSON
    #[arg(long, global = true)]
    pub json: bool,

    /// Limit human-readable output to approximately N tokens (ignored with --json)
    #[arg(long, global = true)]
    pub tokens: Option<u32>,

    /// Path to the cartog database (overrides .cartog.toml and auto-detection).
    /// Can also be set via the CARTOG_DB environment variable.
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
    TypeAlias,
    Trait,
    Module,
    Document,
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
            SymbolKindFilter::TypeAlias => SymbolKind::TypeAlias,
            SymbolKindFilter::Trait => SymbolKind::Trait,
            SymbolKindFilter::Module => SymbolKind::Module,
            SymbolKindFilter::Document => SymbolKind::Document,
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
    },

    /// File-level import dependencies
    Deps {
        /// File path
        file: String,
    },

    /// Index statistics summary
    Stats,

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

        /// Maximum results to return (default: 30, max: 100)
        #[arg(long, default_value = "30")]
        limit: u32,
    },

    /// Token-budget-aware codebase summary (file tree + top symbols by centrality)
    Map {
        /// Approximate token budget for the output (default: 4000)
        #[arg(long, default_value = "4000")]
        tokens: u32,
    },

    /// Show symbols affected by recent git changes
    Changes {
        /// Number of recent commits to consider (default: 5)
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

        /// Enable automatic RAG embedding after index
        #[arg(long)]
        rag: bool,

        /// Delay in seconds before batch embedding after last index
        #[arg(long, default_value = "30")]
        rag_delay: u64,
    },

    /// Start MCP server over stdio (for Claude Code, Cursor, and other MCP clients)
    Serve {
        /// Enable file watching with auto-re-index during MCP session
        #[arg(long)]
        watch: bool,

        /// Enable automatic RAG embedding when watching
        #[arg(long)]
        rag: bool,
    },

    /// Semantic code search (RAG pipeline)
    #[command(subcommand)]
    Rag(RagCommand),

    /// Generate shell completions for bash, zsh, fish, elvish, or powershell.
    ///
    /// Example: `cartog completions bash > ~/.local/share/bash-completion/completions/cartog`
    Completions {
        /// Shell to generate completions for
        shell: clap_complete::Shell,
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
