//! Index-management commands: `stats` (+ `savings`), `map`, `changes`, `push`,
//! `pull`, and `watch`. These operate over the whole index rather than a single
//! symbol.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use serde::Serialize;

use super::shared::{open_db, output, truncate_to_budget};
use super::{mermaid, remote};
use crate::cli::SymbolKindFilter;
use crate::config::CartogConfig;
use cartog_core::Compact;
use cartog_indexer as indexer;
use cartog_rag as rag;
use cartog_watch::{self as watch, WatchConfig};

mod savings;
use savings::{render_savings, savings_scope_label};

/// Upload the local index DB to S3-compatible storage.
pub fn cmd_push(
    db_path: &Path,
    config: &CartogConfig,
    cli_remote: Option<&str>,
    json: bool,
) -> Result<()> {
    remote::push_index(db_path, config, cli_remote, json)
}

/// Download an index DB from S3-compatible storage into the local project.
pub fn cmd_pull(
    db_path: &Path,
    config: &CartogConfig,
    cli_remote: Option<&str>,
    force: bool,
    no_sign_request: bool,
    json: bool,
) -> Result<()> {
    remote::pull_index(db_path, config, cli_remote, force, no_sign_request, json)
}

/// Index statistics summary.
pub fn cmd_stats(
    db_path: &Path,
    json: bool,
    token_budget: Option<u32>,
    embedding_dim: usize,
    savings: bool,
) -> Result<()> {
    let db = open_db(db_path, embedding_dim)?;

    if savings {
        let report = db.savings_breakdown()?;
        let scope = savings_scope_label(db_path);
        return output(&report, json, token_budget, |r| render_savings(&scope, r));
    }

    let stats = db.stats()?;
    output(&stats, json, token_budget, |stats| {
        let mut out = String::new();
        out.push_str(&format!("Files:    {}\n", stats.num_files));
        out.push_str(&format!("Symbols:  {}\n", stats.num_symbols));
        let mut edge_parts = vec![format!("{} resolved", stats.num_resolved)];
        if stats.num_external > 0 {
            edge_parts.push(format!("{} external", stats.num_external));
        }
        if stats.num_unresolvable > 0 {
            edge_parts.push(format!("{} unresolvable", stats.num_unresolvable));
        }
        out.push_str(&format!(
            "Edges:    {} ({})\n",
            stats.num_edges,
            edge_parts.join(", ")
        ));
        if !stats.languages.is_empty() {
            out.push_str("Languages:\n");
            for (lang, count) in &stats.languages {
                out.push_str(&format!("  {lang}: {count} files\n"));
            }
        }
        if !stats.symbol_kinds.is_empty() {
            out.push_str("Symbols by kind:\n");
            for (kind, count) in &stats.symbol_kinds {
                out.push_str(&format!("  {kind}: {count}\n"));
            }
        }
        if stats.num_files == 0 {
            out.push_str("\nIndex is empty — run `cartog index .` to build the code graph.\n");
        }
        out
    })
}

/// Token-budget-aware codebase summary: file tree + top symbols ranked by centrality.
pub fn cmd_map(
    db_path: &Path,
    tokens: u32,
    json: bool,
    compact: bool,
    mermaid: bool,
    embedding_dim: usize,
) -> Result<()> {
    let db = open_db(db_path, embedding_dim)?;
    let files = db.all_files()?;

    if files.is_empty() {
        if json {
            println!("{{}}");
        } else if mermaid {
            // Tell the user to index before pasting the (empty) diagram.
            eprintln!("No files indexed. Run `cartog index .` first.");
            println!("graph TD\n    repo[\"Repo (empty)\"]");
        } else {
            println!("No files indexed. Run 'cartog index .' first.");
        }
        return Ok(());
    }

    // Log AFTER the empty-files guard so no-op calls on an unindexed repo
    // don't inflate `cartog savings`.
    db.log_query("map", "cli");

    if mermaid && !json {
        // Honor the token budget by walking files until we exhaust it. The
        // emitted lines look like:
        //   repo --> f_<sane>_<hash8>["<label>"]
        //   f_<sane>_<hash8> --> s_<sane>_<hash8>["<name> (<kind>)"]
        // so per-file overhead is roughly `len(path) * 2 + len(prefix+hash) * 2 + 30`
        // and per-leaf overhead is roughly `len(path) + len(name) * 2 + len(kind) + 50`.
        // The constants are deliberately conservative — better to underfill
        // than overshoot the documented `--tokens` budget.
        let budget_bytes = (tokens as usize) * 4;
        let mut included_files: Vec<&str> = Vec::new();
        let mut size = "graph TD\n    repo[\"Repo\"]\n".len();
        // f_<sanitized>_<hash8> has at least len(path)+13 bytes of ID overhead.
        const FILE_ID_OVERHEAD: usize = 13;
        // s_<sanitized>_<hash8> for a "<file>::<name>" raw key — even longer.
        const SYM_ID_OVERHEAD: usize = 13;
        for f in &files {
            let edge_cost = f.len() * 2 + FILE_ID_OVERHEAD + 30;
            if size + edge_cost > budget_bytes && !included_files.is_empty() {
                break;
            }
            size += edge_cost;
            included_files.push(f.as_str());
        }
        // HashSet so per-symbol membership is O(1), not O(N).
        let included_set: std::collections::HashSet<&str> =
            included_files.iter().copied().collect();
        // Add top symbols per file until budget runs out.
        let symbols = db.top_symbols(500)?;
        let mut symbols_by_file: std::collections::BTreeMap<String, Vec<(String, String)>> =
            std::collections::BTreeMap::new();
        for sym in &symbols {
            if !included_set.contains(sym.file_path.as_str()) {
                continue;
            }
            // The actual emitted leaf carries the file path inside the
            // sym ID (`s_<sanitize(file::name)>_<hash>`), plus the file ID
            // again on the source side of `-->`. Account for both.
            let leaf_cost = sym.name.len() * 2 + sym.file_path.len() + SYM_ID_OVERHEAD * 2 + 50;
            if size + leaf_cost > budget_bytes {
                break;
            }
            size += leaf_cost;
            symbols_by_file
                .entry(sym.file_path.clone())
                .or_default()
                .push((sym.name.clone(), sym.kind.as_str().to_string()));
        }
        let included_owned: Vec<String> = included_files.iter().map(|s| (*s).to_string()).collect();
        let symbols_vec: Vec<(String, Vec<(String, String)>)> =
            symbols_by_file.into_iter().collect();
        print!("{}", mermaid::render_map(&included_owned, &symbols_vec));
        return Ok(());
    }

    if json {
        // For JSON, return structured data without budget constraints
        let mut symbols = db.top_symbols(200)?;
        if compact {
            symbols.compact_in_place();
        }

        #[derive(Serialize)]
        struct MapResult {
            files: Vec<String>,
            top_symbols: Vec<cartog_core::Symbol>,
        }

        let result = MapResult {
            files,
            top_symbols: symbols,
        };
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }

    // Human-readable: build file tree, then fill remaining budget with symbols
    let budget_bytes = (tokens as usize) * 4;

    // Phase 1: file tree
    let mut out = String::new();
    out.push_str(&format!("# Codebase Map ({} files)\n\n", files.len()));
    for file in &files {
        out.push_str(&format!("  {file}\n"));
    }

    let tree_bytes = out.len();
    let remaining = budget_bytes.saturating_sub(tree_bytes);

    if remaining < 100 {
        print!("{}", truncate_to_budget(&out, tokens));
        return Ok(());
    }

    // Phase 2: top symbols by centrality, grouped by file
    out.push_str("\n# Top Symbols (by reference count)\n\n");

    let symbols = db.top_symbols(500)?;
    let mut current_file = "";

    for sym in &symbols {
        if out.len() >= budget_bytes {
            break;
        }

        if sym.file_path != current_file {
            let header = format!("\n{}:\n", sym.file_path);
            if out.len() + header.len() > budget_bytes {
                break;
            }
            out.push_str(&header);
            current_file = &sym.file_path;
        }

        let sig = sym.signature.as_deref().unwrap_or("");
        let line = format!(
            "  {kind} {name}{sig}  L{start}-{end}  ({refs} refs)\n",
            kind = sym.kind,
            name = sym.name,
            start = sym.start_line,
            end = sym.end_line,
            refs = sym.in_degree,
        );

        if out.len() + line.len() > budget_bytes {
            break;
        }
        out.push_str(&line);
    }

    print!("{out}");
    Ok(())
}

/// Show symbols affected by recent git changes.
pub fn cmd_changes(
    db_path: &Path,
    commits: u32,
    kind: Option<SymbolKindFilter>,
    json: bool,
    compact: bool,
    token_budget: Option<u32>,
    embedding_dim: usize,
) -> Result<()> {
    let db = open_db(db_path, embedding_dim)?;
    let root = std::env::current_dir()?;

    // Log AFTER the git call succeeds; otherwise non-git directories inflate
    // the savings counter via the `?` propagating an error.
    let changed_files = indexer::git_recently_changed_files(&root, commits)?;
    db.log_query("changes", "cli");

    if changed_files.is_empty() {
        if json {
            println!("[]");
        } else {
            println!("No files changed in the last {commits} commits.");
        }
        return Ok(());
    }

    let kind_filter = match kind {
        Some(SymbolKindFilter::All) | None => None,
        Some(k) => Some(cartog_core::SymbolKind::from(k)),
    };
    let mut symbols = db.symbols_for_files(&changed_files, kind_filter)?;
    if compact {
        symbols.compact_in_place();
    }

    let result = cartog_core::ChangesResult {
        changed_files,
        symbols,
    };

    output(&result, json, token_budget, |r| {
        let mut out = format!(
            "{} files changed in last {} commits, {} symbols affected\n\n",
            r.changed_files.len(),
            commits,
            r.symbols.len()
        );
        let mut current_file = "";
        for sym in &r.symbols {
            if sym.file_path != current_file {
                current_file = &sym.file_path;
                out.push_str(&format!("{current_file}:\n"));
            }
            let sig = sym.signature.as_deref().unwrap_or("");
            out.push_str(&format!(
                "  {kind} {name}{sig}  L{start}-{end}\n",
                kind = sym.kind,
                name = sym.name,
                start = sym.start_line,
                end = sym.end_line,
            ));
        }
        let files_with_symbols: std::collections::HashSet<&str> =
            r.symbols.iter().map(|s| s.file_path.as_str()).collect();
        let unindexed: Vec<_> = r
            .changed_files
            .iter()
            .filter(|f| !files_with_symbols.contains(f.as_str()))
            .collect();
        if !unindexed.is_empty() {
            out.push_str(&format!(
                "\n{} changed files not in index:\n",
                unindexed.len()
            ));
            for f in unindexed {
                out.push_str(&format!("  {f}\n"));
            }
        }
        out
    })
}

/// Watch for file changes and auto-re-index.
#[allow(clippy::too_many_arguments)]
pub fn cmd_watch(
    db_path: &Path,
    path: &str,
    debounce: u64,
    rag_override: Option<bool>,
    rag_delay: u64,
    provider_config: rag::EmbeddingProviderConfig,
    redact: indexer::RedactionConfig,
    filter: indexer::WalkFilter,
    allow_create: bool,
    json: bool,
) -> Result<()> {
    let mut config = WatchConfig::new(PathBuf::from(path));
    config.debounce = Duration::from_secs(debounce);
    config.rag_override = rag_override;
    config.rag_delay = Duration::from_secs(rag_delay);
    config.rag_config = provider_config;
    config.redact = redact;
    config.walk_filter = filter;
    config.allow_create = allow_create;
    config.json_events = json;
    // pid_lock_dir/slot must be both-or-neither: a sandboxed host with no
    // resolvable state dir falls back to untracked mode rather than hard-
    // failing on the inverse half-config check in validate_pid_lock_config.
    config.pid_lock_dir = crate::state::default_state_dir();
    config.pid_lock_slot = config
        .pid_lock_dir
        .as_ref()
        .map(|_| crate::state::slot_for_db("watch", db_path));

    let db_path_str = db_path.to_string_lossy();
    watch::run_watch(config, &db_path_str)
}

#[cfg(test)]
mod tests {
    use super::super::test_support::indexed_db;
    use super::*;

    #[test]
    fn cmd_stats_plain_json_and_savings_branches_do_not_error() {
        let (_tmp, db) = indexed_db();
        cmd_stats(&db, false, None, 384, false).expect("stats ok");
        cmd_stats(&db, true, None, 384, false).expect("stats --json ok");
        cmd_stats(&db, false, None, 384, true).expect("stats --savings ok");
    }

    #[test]
    fn cmd_map_plain_json_and_mermaid_branches_do_not_error() {
        let (_tmp, db) = indexed_db();
        cmd_map(&db, 1000, false, false, false, 384).expect("map ok");
        cmd_map(&db, 1000, true, false, false, 384).expect("map --json ok");
        cmd_map(&db, 1000, true, true, false, 384).expect("map --json --compact ok");
        cmd_map(&db, 1000, false, false, true, 384).expect("map --mermaid ok");
    }
}
