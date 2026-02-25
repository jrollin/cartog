use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::db::Database;
use crate::indexer;
use crate::types::SymbolKind;

const DB_FILE: &str = ".cartog.db";

fn open_db() -> Result<Database> {
    Database::open(DB_FILE).context("Failed to open cartog database")
}

/// Print `data` as pretty JSON if `json` is true, otherwise call `human_fmt`.
fn output<T: Serialize>(data: &T, json: bool, human_fmt: impl FnOnce(&T)) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(data)?);
    } else {
        human_fmt(data);
    }
    Ok(())
}

/// Build or rebuild the code graph index.
pub fn cmd_index(path: &str, force: bool, json: bool) -> Result<()> {
    let root = Path::new(path);
    let db = open_db()?;

    let result = indexer::index_directory(&db, root, force)?;

    output(&result, json, |r| {
        println!(
            "Indexed {} files ({} skipped, {} removed)",
            r.files_indexed, r.files_skipped, r.files_removed
        );
        println!(
            "  {} symbols, {} edges ({} resolved)",
            r.symbols_added, r.edges_added, r.edges_resolved
        );
    })
}

/// Show symbols and structure of a file.
pub fn cmd_outline(file: &str, json: bool) -> Result<()> {
    let db = open_db()?;
    let symbols = db.outline(file)?;

    output(&symbols, json, |syms| {
        if syms.is_empty() {
            println!("No symbols found in {file}");
            return;
        }
        for sym in syms {
            let indent = if sym.parent_id.is_some() { "  " } else { "" };
            let async_prefix = if sym.is_async { "async " } else { "" };
            match sym.kind {
                SymbolKind::Import => {
                    let text = sym.signature.as_deref().unwrap_or(&sym.name);
                    println!("{indent}{text}  L{}", sym.start_line);
                }
                _ => {
                    let sig = sym.signature.as_deref().unwrap_or("");
                    println!(
                        "{indent}{async_prefix}{kind} {name}{sig}  L{start}-{end}",
                        kind = sym.kind,
                        name = sym.name,
                        start = sym.start_line,
                        end = sym.end_line,
                    );
                }
            }
        }
    })
}

/// Find all callers of a symbol.
pub fn cmd_callers(name: &str, json: bool) -> Result<()> {
    let db = open_db()?;
    let results = db.callers(name)?;

    if json {
        let items: Vec<_> = results
            .iter()
            .map(|(edge, sym)| {
                serde_json::json!({
                    "edge": edge,
                    "caller": sym,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
    } else {
        if results.is_empty() {
            println!("No callers found for '{name}'");
            return Ok(());
        }
        for (edge, sym) in &results {
            let caller_name = sym
                .as_ref()
                .map(|s| s.name.as_str())
                .unwrap_or(&edge.source_id);
            println!(
                "{kind}  {caller}  {file}:{line}",
                kind = edge.kind,
                caller = caller_name,
                file = edge.file_path,
                line = edge.line,
            );
        }
    }

    Ok(())
}

/// Find what a symbol calls.
pub fn cmd_callees(name: &str, json: bool) -> Result<()> {
    let db = open_db()?;
    let edges = db.callees(name)?;

    output(&edges, json, |edges| {
        if edges.is_empty() {
            println!("No callees found for '{name}'");
            return;
        }
        for edge in edges {
            println!(
                "{target}  {file}:{line}",
                target = edge.target_name,
                file = edge.file_path,
                line = edge.line,
            );
        }
    })
}

/// Transitive impact analysis — what breaks if this changes?
pub fn cmd_impact(name: &str, depth: u32, json: bool) -> Result<()> {
    let db = open_db()?;
    let results = db.impact(name, depth)?;

    if json {
        let items: Vec<_> = results
            .iter()
            .map(|(edge, d)| {
                serde_json::json!({
                    "edge": edge,
                    "depth": d,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
    } else {
        if results.is_empty() {
            println!("No impact found for '{name}'");
            return Ok(());
        }
        for (edge, depth) in &results {
            let indent = "  ".repeat(*depth as usize);
            println!(
                "{indent}{kind}  {source}  {file}:{line}",
                kind = edge.kind,
                source = edge.source_id,
                file = edge.file_path,
                line = edge.line,
            );
        }
    }

    Ok(())
}

/// All references to a symbol (calls, imports, inherits).
pub fn cmd_refs(name: &str, json: bool) -> Result<()> {
    let db = open_db()?;
    let edges = db.refs(name)?;

    output(&edges, json, |edges| {
        if edges.is_empty() {
            println!("No references found for '{name}'");
            return;
        }
        for edge in edges {
            println!(
                "{kind}  {source}  {file}:{line}",
                kind = edge.kind,
                source = edge.source_id,
                file = edge.file_path,
                line = edge.line,
            );
        }
    })
}

/// Show inheritance hierarchy for a class.
pub fn cmd_hierarchy(name: &str, json: bool) -> Result<()> {
    let db = open_db()?;
    let pairs = db.hierarchy(name)?;

    if json {
        let items: Vec<_> = pairs
            .iter()
            .map(|(child, parent)| {
                serde_json::json!({
                    "child": child,
                    "parent": parent,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
    } else {
        if pairs.is_empty() {
            println!("No hierarchy found for '{name}'");
            return Ok(());
        }
        for (child, parent) in &pairs {
            println!("{child} -> {parent}");
        }
    }

    Ok(())
}

/// File-level import dependencies.
pub fn cmd_deps(file: &str, json: bool) -> Result<()> {
    let db = open_db()?;
    let edges = db.file_deps(file)?;

    output(&edges, json, |edges| {
        if edges.is_empty() {
            println!("No dependencies found for '{file}'");
            return;
        }
        for edge in edges {
            println!(
                "{target}  L{line}",
                target = edge.target_name,
                line = edge.line
            );
        }
    })
}

/// Index statistics summary.
pub fn cmd_stats(json: bool) -> Result<()> {
    let db = open_db()?;
    let stats = db.stats()?;

    output(&stats, json, |stats| {
        println!("Files:    {}", stats.num_files);
        println!("Symbols:  {}", stats.num_symbols);
        println!(
            "Edges:    {} ({} resolved)",
            stats.num_edges, stats.num_resolved
        );
        if !stats.languages.is_empty() {
            println!("Languages:");
            for (lang, count) in &stats.languages {
                println!("  {lang}: {count} files");
            }
        }
        if !stats.symbol_kinds.is_empty() {
            println!("Symbols by kind:");
            for (kind, count) in &stats.symbol_kinds {
                println!("  {kind}: {count}");
            }
        }
    })
}
