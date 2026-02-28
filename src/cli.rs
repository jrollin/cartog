use clap::{Parser, Subcommand, ValueEnum};

use crate::types::{EdgeKind, SymbolKind};

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
}

/// Filter for symbol kinds in the search command.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum SymbolKindFilter {
    Function,
    Class,
    Method,
    Variable,
    Import,
}

impl From<SymbolKindFilter> for SymbolKind {
    fn from(f: SymbolKindFilter) -> Self {
        match f {
            SymbolKindFilter::Function => SymbolKind::Function,
            SymbolKindFilter::Class => SymbolKind::Class,
            SymbolKindFilter::Method => SymbolKind::Method,
            SymbolKindFilter::Variable => SymbolKind::Variable,
            SymbolKindFilter::Import => SymbolKind::Import,
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
}

impl From<EdgeKindFilter> for EdgeKind {
    fn from(f: EdgeKindFilter) -> Self {
        match f {
            EdgeKindFilter::Calls => EdgeKind::Calls,
            EdgeKindFilter::Imports => EdgeKind::Imports,
            EdgeKindFilter::Inherits => EdgeKind::Inherits,
            EdgeKindFilter::References => EdgeKind::References,
            EdgeKindFilter::Raises => EdgeKind::Raises,
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

    /// Watch for file changes and auto-re-index
    Watch {
        /// Directory to watch (defaults to current directory)
        #[arg(default_value = ".")]
        path: String,

        /// Debounce window in seconds
        #[arg(long, default_value = "2")]
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
