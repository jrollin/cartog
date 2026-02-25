use clap::{Parser, Subcommand};

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

    /// Find all callers of a symbol
    Callers {
        /// Symbol name to search for
        name: String,
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

    /// All references to a symbol (calls, imports, inherits)
    Refs {
        /// Symbol name to search for
        name: String,
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
}
