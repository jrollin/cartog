//! Graph-navigation commands: `outline`, `callees`, `impact`, `trace`, `refs`,
//! `hierarchy`, `deps`. Each opens the DB, runs one query, logs it (only on a
//! hit, so no-op calls don't inflate `cartog savings`), and renders human/JSON
//! output with the shared `did_you_mean`/`empty_index_hint` diagnostics.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Serialize;

use super::mermaid;
use super::shared::{did_you_mean, empty_index_hint, open_db, output};
use crate::cli::EdgeKindFilter;
use cartog_core::{Compact, EdgeKind, SymbolKind};

/// Show symbols and structure of a file.
pub fn cmd_outline(
    db_path: &Path,
    file: &str,
    json: bool,
    compact: bool,
    token_budget: Option<u32>,
    embedding_dim: usize,
) -> Result<()> {
    let db = open_db(db_path, embedding_dim)?;
    let mut symbols = db.outline(file)?;
    // Don't count empty results — an empty-index call or a typo'd file path
    // didn't actually save the user any tokens vs grep + read.
    if !symbols.is_empty() {
        db.log_query("outline", "cli");
    }
    if compact {
        symbols.compact_in_place();
    }
    let file = file.to_string();
    output(&symbols, json, token_budget, |syms| {
        if syms.is_empty() {
            return format!("No symbols found in {file}{}\n", empty_index_hint(&db));
        }
        let mut out = String::new();
        for sym in syms {
            let indent = if sym.parent_id.is_some() { "  " } else { "" };
            let async_prefix = if sym.is_async { "async " } else { "" };
            match sym.kind {
                SymbolKind::Import => {
                    let text = sym.signature.as_deref().unwrap_or(&sym.name);
                    out.push_str(&format!("{indent}{text}  L{}\n", sym.start_line));
                }
                _ => {
                    let sig = sym.signature.as_deref().unwrap_or("");
                    out.push_str(&format!(
                        "{indent}{async_prefix}{kind} {name}{sig}  L{start}-{end}\n",
                        kind = sym.kind,
                        name = sym.name,
                        start = sym.start_line,
                        end = sym.end_line,
                    ));
                }
            }
        }
        out
    })
}

/// Find what a symbol calls.
pub fn cmd_callees(
    db_path: &Path,
    name: &str,
    json: bool,
    token_budget: Option<u32>,
    embedding_dim: usize,
) -> Result<()> {
    let db = open_db(db_path, embedding_dim)?;
    let edges = db.callees(name)?;
    if !edges.is_empty() {
        db.log_query("callees", "cli");
    }
    let name = name.to_string();
    output(&edges, json, token_budget, |edges| {
        if edges.is_empty() {
            return format!(
                "No callees found for '{name}'{}{}\n",
                empty_index_hint(&db),
                did_you_mean(&db, &name)
            );
        }
        let mut out = String::new();
        for edge in edges {
            out.push_str(&format!(
                "{target}  {file}:{line}\n",
                target = edge.target_name,
                file = edge.file_path,
                line = edge.line,
            ));
        }
        out
    })
}

/// Transitive impact analysis — what breaks if this changes?
pub fn cmd_impact(
    db_path: &Path,
    name: &str,
    depth: u32,
    json: bool,
    token_budget: Option<u32>,
    embedding_dim: usize,
) -> Result<()> {
    let db = open_db(db_path, embedding_dim)?;
    let results = db.impact(name, depth)?;
    if !results.is_empty() {
        db.log_query("impact", "cli");
    }
    let name = name.to_string();

    #[derive(Serialize)]
    struct ImpactEntry {
        edge: cartog_core::Edge,
        depth: u32,
    }

    let items: Vec<ImpactEntry> = results
        .into_iter()
        .map(|(edge, d)| ImpactEntry { edge, depth: d })
        .collect();

    output(&items, json, token_budget, |items| {
        if items.is_empty() {
            return format!(
                "No impact found for '{name}'{}{}\n",
                empty_index_hint(&db),
                did_you_mean(&db, &name)
            );
        }
        let mut out = String::new();
        for entry in items {
            let indent = "  ".repeat(entry.depth as usize);
            out.push_str(&format!(
                "{indent}{kind}  {source}  {file}:{line}\n",
                kind = entry.edge.kind,
                source = entry.edge.source_id,
                file = entry.edge.file_path,
                line = entry.edge.line,
            ));
        }
        out
    })
}

/// Find a call path between two symbols, with each hop's body inline.
#[allow(clippy::too_many_arguments)]
pub fn cmd_trace(
    db_path: &Path,
    from: &str,
    to: &str,
    depth: u32,
    json: bool,
    compact: bool,
    token_budget: Option<u32>,
    embedding_dim: usize,
) -> Result<()> {
    const MAX_TRACE_DEPTH: u32 = 20;
    let db = open_db(db_path, embedding_dim)?;
    // `file_path` is stored relative to the index root. The DB lives at
    // `<root>/.cartog/db.sqlite`, so the root is the db's grandparent — robust
    // regardless of the cwd `cartog trace` was launched from.
    let index_root = index_root_from_db_path(db_path);
    let path = db.trace(from, to, depth.min(MAX_TRACE_DEPTH))?;
    if path.is_some() {
        db.log_query("trace", "cli");
    }

    #[derive(Serialize)]
    struct HydratedHop {
        source_name: String,
        target_name: String,
        kind: cartog_core::EdgeKind,
        file_path: String,
        line: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        body: Option<String>,
    }

    // `{found, hops}` so `--json` distinguishes "no path" (found=false) from
    // "from == to" (found=true, hops=[]) — matching the cartog_trace MCP shape.
    #[derive(Serialize)]
    struct TraceResult {
        from: String,
        to: String,
        found: bool,
        hops: Vec<HydratedHop>,
    }

    let hops: Vec<HydratedHop> = path
        .iter()
        .flatten()
        .map(|h| HydratedHop {
            // Compact drops the inline body (the heaviest part of a trace).
            body: if compact {
                None
            } else {
                hop_body(&db, &index_root, &h.source_id)
            },
            source_name: h.source_name.clone(),
            target_name: h.target_name.clone(),
            kind: h.kind,
            file_path: h.file_path.clone(),
            line: h.line,
        })
        .collect();

    let result = TraceResult {
        from: from.to_string(),
        to: to.to_string(),
        found: path.is_some(),
        hops,
    };
    output(&result, json, token_budget, |r| {
        if !r.found {
            return format!(
                "No call path from '{from}' to '{to}'{}{}{}\n",
                empty_index_hint(&db),
                did_you_mean(&db, &r.from),
                did_you_mean(&db, &r.to),
            );
        }
        if r.hops.is_empty() {
            return format!("'{}' is the target.\n", r.from);
        }
        let mut out = String::new();
        for hop in &r.hops {
            out.push_str(&format!(
                "{src} → {dst}  {file}:{line}\n",
                src = hop.source_name,
                dst = hop.target_name,
                file = hop.file_path,
                line = hop.line,
            ));
            if let Some(body) = &hop.body {
                out.push_str(body);
                out.push('\n');
            }
        }
        out
    })
}

/// Index root for a DB at `<root>/.cartog/db.sqlite` — the db's grandparent.
/// Falls back to the db's own parent (legacy `.cartog.db` layout), then `.`.
fn index_root_from_db_path(db_path: &Path) -> PathBuf {
    db_path
        .parent()
        .and_then(Path::parent)
        .or_else(|| db_path.parent())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Body of the symbol with id `source_id` — the exact symbol on the call path.
/// Prefers stored RAG content (redaction-aware), else reads source by byte
/// range from `root`-relative `file_path`. `None` when neither is available
/// (header-only hop).
fn hop_body(db: &cartog_db::Database, root: &Path, source_id: &str) -> Option<String> {
    if let Some((content, _)) = db.get_symbol_content(source_id).ok().flatten() {
        return Some(content);
    }
    let sym = db
        .get_symbols_by_ids(std::slice::from_ref(&source_id.to_string()))
        .ok()?
        .into_iter()
        .next()?;
    source_slice(
        root,
        &sym.file_path,
        sym.start_byte as usize,
        sym.end_byte as usize,
    )
}

/// Read `path` (resolved against `root`; absolute paths pass through) and return
/// the `[start, end)` byte slice, snapped to char boundaries. `None` on any read
/// or range error.
fn source_slice(root: &Path, path: &str, mut start: usize, mut end: usize) -> Option<String> {
    let src = std::fs::read_to_string(root.join(path)).ok()?;
    if start >= end || end > src.len() {
        return None;
    }
    while start < end && !src.is_char_boundary(start) {
        start += 1;
    }
    while end > start && !src.is_char_boundary(end) {
        end -= 1;
    }
    Some(src[start..end].to_string())
}

/// All references to a symbol (calls, imports, inherits, references, raises).
pub fn cmd_refs(
    db_path: &Path,
    name: &str,
    kind: Option<EdgeKindFilter>,
    json: bool,
    compact: bool,
    token_budget: Option<u32>,
    embedding_dim: usize,
) -> Result<()> {
    let db = open_db(db_path, embedding_dim)?;
    let kind_filter = kind.map(EdgeKind::from);
    let results = db.refs(name, kind_filter)?;
    if !results.is_empty() {
        db.log_query("refs", "cli");
    }
    let name = name.to_string();

    #[derive(Serialize)]
    struct RefEntry {
        edge: cartog_core::Edge,
        source: Option<cartog_core::Symbol>,
    }

    let items: Vec<RefEntry> = results
        .into_iter()
        .map(|(edge, mut sym)| {
            if compact {
                if let Some(s) = sym.as_mut() {
                    s.compact_in_place();
                }
            }
            RefEntry { edge, source: sym }
        })
        .collect();

    output(&items, json, token_budget, |items| {
        if items.is_empty() {
            return format!(
                "No references found for '{name}'{}{}\n",
                empty_index_hint(&db),
                did_you_mean(&db, &name)
            );
        }
        let mut out = String::new();
        for entry in items {
            let source_name = entry
                .source
                .as_ref()
                .map(|s| s.name.as_str())
                .unwrap_or(&entry.edge.source_id);
            out.push_str(&format!(
                "{kind}  {source}  {file}:{line}\n",
                kind = entry.edge.kind,
                source = source_name,
                file = entry.edge.file_path,
                line = entry.edge.line,
            ));
        }
        out
    })
}

/// Show inheritance hierarchy for a class.
pub fn cmd_hierarchy(
    db_path: &Path,
    name: &str,
    json: bool,
    mermaid: bool,
    token_budget: Option<u32>,
    embedding_dim: usize,
) -> Result<()> {
    let db = open_db(db_path, embedding_dim)?;
    let pairs = db.hierarchy(name)?;
    if !pairs.is_empty() {
        db.log_query("hierarchy", "cli");
    }
    let name = name.to_string();

    // --json wins if both flags are set (matches the documented behavior).
    if mermaid && !json {
        // Surface the same diagnostic the plain branch shows so users running
        // --mermaid on a typo or empty index get the did-you-mean hint
        // alongside the bare `graph TD` document.
        if pairs.is_empty() {
            eprintln!(
                "No hierarchy found for '{name}'{}{}",
                empty_index_hint(&db),
                did_you_mean(&db, &name)
            );
        }
        print!("{}", mermaid::render_hierarchy(&pairs));
        return Ok(());
    }

    #[derive(Serialize)]
    struct HierarchyEntry {
        child: String,
        parent: String,
    }

    let items: Vec<HierarchyEntry> = pairs
        .into_iter()
        .map(|(child, parent)| HierarchyEntry { child, parent })
        .collect();

    output(&items, json, token_budget, |items| {
        if items.is_empty() {
            return format!(
                "No hierarchy found for '{name}'{}{}\n",
                empty_index_hint(&db),
                did_you_mean(&db, &name)
            );
        }
        let mut out = String::new();
        for entry in items {
            out.push_str(&format!("{} -> {}\n", entry.child, entry.parent));
        }
        out
    })
}

/// File-level import dependencies.
pub fn cmd_deps(
    db_path: &Path,
    file: &str,
    json: bool,
    mermaid: bool,
    token_budget: Option<u32>,
    embedding_dim: usize,
) -> Result<()> {
    let db = open_db(db_path, embedding_dim)?;
    let edges = db.file_deps(file)?;
    if !edges.is_empty() {
        db.log_query("deps", "cli");
    }
    let file = file.to_string();

    if mermaid && !json {
        // Surface the same diagnostic the plain branch shows so users running
        // --mermaid against an unindexed file or empty index get the hint.
        if edges.is_empty() {
            eprintln!(
                "No dependencies found for '{file}'{}",
                empty_index_hint(&db)
            );
        }
        let targets: Vec<(String, u32)> = edges
            .iter()
            .map(|e| (e.target_name.clone(), e.line))
            .collect();
        print!("{}", mermaid::render_deps(&file, &targets));
        return Ok(());
    }

    output(&edges, json, token_budget, |edges| {
        if edges.is_empty() {
            return format!(
                "No dependencies found for '{file}'{}\n",
                empty_index_hint(&db)
            );
        }
        let mut out = String::new();
        for edge in edges {
            out.push_str(&format!(
                "{target}  L{line}\n",
                target = edge.target_name,
                line = edge.line
            ));
        }
        out
    })
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{indexed_db, queries_logged};
    use super::*;

    // ── cmd_* command bodies over a real indexed DB ───────────────────
    //
    // Drive the read commands end-to-end against a temp DB populated from a
    // small Python fixture. The commands print to stdout (so output content
    // can't be asserted directly), but calling them exercises the real query,
    // the human/JSON formatter closures, the empty-result did-you-mean paths,
    // and the token-budget branch — returning Ok/Err is the observable
    // contract. Query-log side effects are verified via savings_breakdown.

    #[test]
    fn cmd_outline_runs_a_query_for_a_populated_file() {
        let (_tmp, db) = indexed_db();
        let before = queries_logged(&db);
        cmd_outline(&db, "lib.py", false, false, None, 384).expect("outline ok");
        assert_eq!(
            queries_logged(&db),
            before + 1,
            "outline of a populated file must run exactly one query"
        );
    }

    #[test]
    fn cmd_outline_json_branch_does_not_error() {
        let (_tmp, db) = indexed_db();
        cmd_outline(&db, "lib.py", true, false, None, 384).expect("outline --json ok");
        cmd_outline(&db, "lib.py", true, true, None, 384).expect("outline --json --compact ok");
    }

    #[test]
    fn cmd_outline_unknown_file_does_not_error() {
        let (_tmp, db) = indexed_db();
        cmd_outline(&db, "missing.py", false, false, None, 384)
            .expect("outline of unknown file is ok");
    }

    #[test]
    fn cmd_refs_runs_a_query_per_invocation_with_and_without_kind_filter() {
        let (_tmp, db) = indexed_db();
        let before = queries_logged(&db);
        cmd_refs(&db, "helper", None, false, false, None, 384).expect("refs ok");
        cmd_refs(
            &db,
            "helper",
            Some(EdgeKindFilter::Calls),
            false,
            false,
            None,
            384,
        )
        .expect("refs --kind calls ok");
        assert_eq!(
            queries_logged(&db),
            before + 2,
            "each refs invocation must run a query"
        );
    }

    #[test]
    fn cmd_refs_near_miss_name_takes_the_did_you_mean_branch_without_error() {
        let (_tmp, db) = indexed_db();
        // Empty result triggers the did_you_mean / empty_index_hint branch.
        cmd_refs(&db, "helpe", None, false, false, None, 384)
            .expect("refs of near-miss name is ok");
    }

    #[test]
    fn cmd_callees_logs_a_query_only_when_it_finds_results() {
        let (_tmp, db) = indexed_db();
        let before = queries_logged(&db);
        cmd_callees(&db, "main", false, None, 384).expect("callees ok");
        let after_hit = queries_logged(&db);
        cmd_callees(&db, "no_such_symbol", false, None, 384).expect("empty callees is ok");
        let after_miss = queries_logged(&db);

        assert_eq!(after_hit, before + 1, "a callees hit logs one query");
        assert_eq!(
            after_miss, after_hit,
            "an empty callees result must not log a query"
        );
    }

    #[test]
    fn cmd_impact_plain_and_json_branches_do_not_error() {
        let (_tmp, db) = indexed_db();
        cmd_impact(&db, "helper", 3, false, None, 384).expect("impact ok");
        cmd_impact(&db, "helper", 3, true, None, 384).expect("impact --json ok");
    }

    #[test]
    fn cmd_trace_logs_a_query_only_when_a_path_is_found() {
        let (_tmp, db) = indexed_db();
        let before = queries_logged(&db);
        cmd_trace(&db, "speak", "helper", 8, false, false, None, 384).expect("trace ok");
        let after_hit = queries_logged(&db);
        cmd_trace(&db, "speak", "no_such_symbol", 8, false, false, None, 384)
            .expect("no-path trace is ok");
        let after_miss = queries_logged(&db);

        assert_eq!(after_hit, before + 1, "a found path logs one query");
        assert_eq!(
            after_miss, after_hit,
            "a no-path result must not log a query"
        );
    }

    #[test]
    fn cmd_trace_json_branch_does_not_error() {
        let (_tmp, db) = indexed_db();
        cmd_trace(&db, "speak", "helper", 8, true, false, None, 384).expect("trace --json ok");
    }

    #[test]
    fn cmd_trace_json_compact_branch_does_not_error() {
        let (_tmp, db) = indexed_db();
        cmd_trace(&db, "speak", "helper", 8, true, true, None, 384)
            .expect("trace --json --compact ok");
    }

    #[test]
    fn cmd_hierarchy_plain_json_and_mermaid_branches_do_not_error() {
        let (_tmp, db) = indexed_db();
        cmd_hierarchy(&db, "Dog", false, false, None, 384).expect("hierarchy ok");
        cmd_hierarchy(&db, "Dog", true, false, None, 384).expect("hierarchy --json ok");
        cmd_hierarchy(&db, "Dog", false, true, None, 384).expect("hierarchy --mermaid ok");
    }

    #[test]
    fn cmd_deps_plain_and_mermaid_branches_do_not_error() {
        let (_tmp, db) = indexed_db();
        cmd_deps(&db, "lib.py", false, false, None, 384).expect("deps ok");
        cmd_deps(&db, "lib.py", false, true, None, 384).expect("deps --mermaid ok");
    }

    #[test]
    fn index_root_is_db_grandparent() {
        // New layout: <root>/.cartog/db.sqlite → root.
        let root = index_root_from_db_path(Path::new("/proj/.cartog/db.sqlite"));
        assert_eq!(root, Path::new("/proj"));
    }

    #[test]
    fn source_slice_resolves_relative_path_against_root() {
        // file_path is stored relative to the index root; reading must join it
        // to the root, not the process cwd.
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("a.py"), "def f():\n    return 1\n").unwrap();
        // Byte range covering "def f()".
        let body = source_slice(root, "a.py", 0, 7).expect("reads relative to root");
        assert_eq!(body, "def f()");
        // A path that doesn't exist under root → None (not a cwd-relative read).
        assert!(source_slice(root, "missing.py", 0, 5).is_none());
    }
}
