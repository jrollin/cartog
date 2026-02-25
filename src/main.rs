mod cli;
mod commands;
mod db;
mod indexer;
mod languages;
mod types;

use anyhow::Result;
use clap::Parser;

use cli::{Cli, Command};

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Index { path, force } => commands::cmd_index(&path, force, cli.json),
        Command::Outline { file } => commands::cmd_outline(&file, cli.json),
        Command::Callees { name } => commands::cmd_callees(&name, cli.json),
        Command::Impact { name, depth } => commands::cmd_impact(&name, depth, cli.json),
        Command::Refs { name, kind } => commands::cmd_refs(&name, kind, cli.json),
        Command::Hierarchy { name } => commands::cmd_hierarchy(&name, cli.json),
        Command::Deps { file } => commands::cmd_deps(&file, cli.json),
        Command::Stats => commands::cmd_stats(cli.json),
    }
}
