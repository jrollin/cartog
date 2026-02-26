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

        /// Maximum results to return (default: 20, max: 100)
        #[arg(long, default_value = "20")]
        limit: u32,
    },

    /// Start MCP server over stdio (for Claude Code, Cursor, and other MCP clients)
    Serve,
}
