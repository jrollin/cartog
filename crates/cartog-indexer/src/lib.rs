//! Code indexing and change detection for cartog.
//!
//! Walks a directory tree, detects changed files (git diff, SHA-256 hash, or force),
//! extracts symbols and edges via [`cartog_languages`], and writes results to
//! [`cartog_db`]. Uses Merkle tree hashing for surgical symbol-level updates.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use rayon::prelude::*;
use sha2::{Digest, Sha256};
use tracing::warn;
use walkdir::WalkDir;

use cartog_core::{FileInfo, Symbol};
use cartog_db::Database;
use cartog_languages::{detect_language, get_extractor, Extractor};

thread_local! {
    /// Per-worker cache of tree-sitter extractors. Reused across files within
    /// a single rayon worker thread so the Parser is constructed once per
    /// language per thread instead of once per file.
    static THREAD_EXTRACTORS: RefCell<HashMap<&'static str, Box<dyn Extractor>>>
        = RefCell::new(HashMap::new());
}

/// Output of the parallel per-file parse phase.
enum ParseOutput {
    /// Stored hash matched — no re-parse needed.
    Skipped,
    /// File was parsed; caller must run the Merkle-diff + DB write path.
    Parsed {
        rel_path: String,
        lang: &'static str,
        source: String,
        hash: String,
        modified: f64,
        symbols: Vec<Symbol>,
        edges: Vec<cartog_core::Edge>,
    },
    /// Read or extraction failed — already logged; caller increments nothing.
    Failed,
}

fn parse_one_file(
    path: &Path,
    rel_path: &str,
    lang: &'static str,
    force: bool,
    stored_hash: Option<&str>,
) -> ParseOutput {
    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => return ParseOutput::Failed, // binary
        Err(e) => {
            warn!(file = %rel_path, error = %e, "cannot read file");
            return ParseOutput::Failed;
        }
    };

    let hash = file_hash(&source);

    // Hash-based skip (only when not forcing).
    if !force {
        if let Some(old) = stored_hash {
            if old == hash {
                return ParseOutput::Skipped;
            }
        }
    }

    let modified = file_modified(path);

    // Extract symbols/edges using the per-thread extractor cache.
    let extraction = THREAD_EXTRACTORS.with(|cell| {
        let mut map = cell.borrow_mut();
        let extractor = map
            .entry(lang)
            .or_insert_with(|| get_extractor(lang).expect("lang was validated by detect_language"))
            .as_mut();
        extractor.extract(&source, rel_path)
    });

    let mut extraction = match extraction {
        Ok(e) => e,
        Err(err) => {
            warn!(file = %rel_path, error = %err, "extraction failed");
            return ParseOutput::Failed;
        }
    };

    dedup_symbol_ids(&mut extraction.symbols, &mut extraction.edges);
    compute_merkle_hashes(&mut extraction.symbols, &source);

    ParseOutput::Parsed {
        rel_path: rel_path.to_string(),
        lang,
        source,
        hash,
        modified,
        symbols: extraction.symbols,
        edges: extraction.edges,
    }
}

/// Summary of an indexing operation.
#[derive(Debug, Default, serde::Serialize)]
pub struct IndexResult {
    pub files_indexed: u32,
    pub files_skipped: u32,
    pub files_removed: u32,
    pub symbols_added: u32,
    #[serde(skip_serializing_if = "is_zero")]
    pub symbols_modified: u32,
    #[serde(skip_serializing_if = "is_zero")]
    pub symbols_unchanged: u32,
    #[serde(skip_serializing_if = "is_zero")]
    pub symbols_removed: u32,
    pub edges_added: u32,
    pub edges_resolved: u32,
    #[serde(skip_serializing_if = "is_zero")]
    pub edges_lsp_resolved: u32,
}

fn is_zero(v: &u32) -> bool {
    *v == 0
}

/// Index a directory, updating the database incrementally.
///
/// Change detection strategy (in order):
/// 1. `force = true` → re-index everything, no checks
/// 2. Git-based → diff `last_commit..HEAD` to find changed files, skip the rest without reading
/// 3. SHA-256 fallback → read file, hash it, compare to stored hash
pub fn index_directory(db: &Database, root: &Path, force: bool, lsp: bool) -> Result<IndexResult> {
    let mut result = IndexResult::default();

    let root = root.canonicalize().context("Failed to resolve root path")?;

    // Collect files that should be indexed
    let mut current_files = std::collections::HashSet::new();

    // Track files that were actually re-indexed (for scoped edge resolution)
    let mut dirty_files: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Git-based change detection: get set of files changed since last indexed commit
    let last_commit = if force {
        None
    } else {
        db.get_metadata("last_commit")?
    };
    let changed_files = if force {
        None
    } else {
        git_changed_files(&root, last_commit.as_deref())
    };

    // Pre-fetch stored file hashes in one query so workers can decide
    // skip-by-hash without touching SQLite.
    let stored_hashes = if force {
        std::collections::HashMap::new()
    } else {
        db.all_file_hashes().unwrap_or_default()
    };

    // ── Phase 1: walk + filter candidates (cheap, single-threaded) ──
    let mut candidates: Vec<(PathBuf, String, &'static str)> = Vec::new();
    for entry in WalkDir::new(&root)
        .follow_links(true)
        .max_depth(50)
        .into_iter()
        .filter_entry(|e| !is_ignored(e))
    {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                warn!(error = %e, "directory walk error");
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let rel_path = match path.strip_prefix(&root) {
            Ok(p) => p.to_string_lossy().to_string(),
            Err(_) => continue,
        };
        let lang = match detect_language(Path::new(&rel_path)) {
            Some(l) => l,
            None => continue,
        };
        current_files.insert(rel_path.clone());

        // Git-based skip: files not in the changed set and already indexed stay put.
        if !force {
            if let Some(ref changed) = changed_files {
                if !changed.contains(&rel_path) && stored_hashes.contains_key(&rel_path) {
                    result.files_skipped += 1;
                    continue;
                }
            }
        }

        candidates.push((path.to_path_buf(), rel_path, lang));
    }

    // ── Phase 2: parallel parse + extract (CPU-bound, rayon-worker pool) ──
    let parsed: Vec<ParseOutput> = candidates
        .par_iter()
        .map(|(abs, rel, lang)| {
            parse_one_file(
                abs,
                rel,
                lang,
                force,
                stored_hashes.get(rel).map(String::as_str),
            )
        })
        .collect();

    // ── Phase 3: sequential DB writes, preserving walk order ──
    for item in parsed {
        let (rel_path, lang, source, hash, modified, symbols, edges) = match item {
            ParseOutput::Skipped => {
                result.files_skipped += 1;
                continue;
            }
            ParseOutput::Failed => continue,
            ParseOutput::Parsed {
                rel_path,
                lang,
                source,
                hash,
                modified,
                symbols,
                edges,
            } => (rel_path, lang, source, hash, modified, symbols, edges),
        };

        // Try Merkle diff against stored hashes
        let old_hashes = db.get_symbol_hashes_for_file(&rel_path)?;
        let has_old_hashes =
            !old_hashes.is_empty() && old_hashes.iter().any(|(_, ch, _)| ch.is_some());

        if has_old_hashes {
            // Merkle diff: surgical updates
            let diff = merkle_diff(&symbols, &old_hashes);

            dirty_files.insert(rel_path.clone());

            db.delete_symbols(&diff.removed)?;
            result.symbols_removed += diff.removed.len() as u32;

            let mut changed: Vec<cartog_core::Symbol> = Vec::with_capacity(
                diff.added.len() + diff.modified.len() + diff.children_changed.len(),
            );
            changed.extend(diff.added.iter().map(|&i| symbols[i].clone()));
            changed.extend(diff.modified.iter().map(|&i| symbols[i].clone()));
            changed.extend(diff.children_changed.iter().map(|&i| symbols[i].clone()));
            db.insert_symbols(&changed)?;

            result.symbols_added += diff.added.len() as u32;
            result.symbols_modified += diff.modified.len() as u32;
            result.symbols_unchanged += diff.unchanged as u32;

            db.clear_edges_for_file(&rel_path)?;
            db.insert_edges(&edges)?;
            result.edges_added += edges.len() as u32;

            let dirty_indices: Vec<usize> = diff
                .added
                .iter()
                .chain(diff.modified.iter())
                .copied()
                .collect();
            let contents: Vec<(String, String, String, String)> = dirty_indices
                .iter()
                .map(|&i| &symbols[i])
                .filter(|sym| sym.kind != cartog_core::SymbolKind::Import)
                .filter_map(|sym| {
                    extract_symbol_content(&source, sym).map(|(content, header)| {
                        (sym.id.clone(), sym.name.clone(), content, header)
                    })
                })
                .collect();
            if !contents.is_empty() {
                db.insert_symbol_contents(&contents)?;
            }
        } else {
            // No stored hashes (first index or post-migration): full insert
            dirty_files.insert(rel_path.clone());
            db.clear_file_data(&rel_path)?;

            db.insert_symbols(&symbols)?;
            db.insert_edges(&edges)?;

            result.symbols_added += symbols.len() as u32;
            result.edges_added += edges.len() as u32;

            let contents: Vec<(String, String, String, String)> = symbols
                .iter()
                .filter(|sym| sym.kind != cartog_core::SymbolKind::Import)
                .filter_map(|sym| {
                    extract_symbol_content(&source, sym).map(|(content, header)| {
                        (sym.id.clone(), sym.name.clone(), content, header)
                    })
                })
                .collect();
            if !contents.is_empty() {
                db.insert_symbol_contents(&contents)?;
            }
        }

        let num_symbols = symbols.len() as u32;

        db.upsert_file(&FileInfo {
            path: rel_path,
            last_modified: modified,
            hash,
            language: lang.to_string(),
            num_symbols,
        })?;

        result.files_indexed += 1;
    }

    // Remove files that no longer exist. Treat deletions as "dirty" so the
    // scoped incremental-repair branch below still runs when the *only* change
    // is a file deletion — otherwise unchanged files keep dangling target_ids
    // and stale in-degrees until the next edit.
    let all_indexed = db.all_files()?;
    for indexed_path in all_indexed {
        if !current_files.contains(&indexed_path) {
            dirty_files.insert(indexed_path.clone());
            db.remove_file(&indexed_path)?;
            result.files_removed += 1;
        }
    }

    // Resolve edges — scoped to dirty files for incremental, global for force/first-index
    if force || dirty_files.len() == current_files.len() {
        result.edges_resolved = db.resolve_edges()?;
        db.compute_in_degrees()?;
    } else if !dirty_files.is_empty() {
        // Invalidate edges from unchanged files that pointed to symbols in dirty files
        // (those symbol IDs may have changed even with stable IDs if a symbol was renamed/removed)
        db.invalidate_edges_targeting(&dirty_files)?;
        result.edges_resolved = db.resolve_edges_scoped(&dirty_files)?;
        db.compute_in_degrees_scoped(&dirty_files)?;
    }

    // LSP-based resolution for edges the heuristic couldn't resolve.
    // Auto-detected when `lsp` feature is compiled in; silently skipped otherwise.
    #[cfg(feature = "lsp")]
    if lsp {
        result.edges_lsp_resolved = cartog_lsp::lsp_resolve_edges(db, &root, None)?;
    }
    #[cfg(not(feature = "lsp"))]
    let _ = lsp; // suppress unused warning when feature is off

    // Store the current git commit as last indexed
    if let Some(commit) = git_head_commit(&root) {
        db.set_metadata("last_commit", &commit)?;
    }

    Ok(result)
}

fn is_ignored(entry: &walkdir::DirEntry) -> bool {
    let name = entry.file_name().to_string_lossy();

    // Skip hidden directories and common non-code directories
    if entry.file_type().is_dir() {
        return is_ignored_dirname(&name);
    }

    false
}

/// Check if a directory name should be ignored during indexing.
///
/// Shared between the walkdir-based indexer and the file watcher.
pub fn is_ignored_dirname(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | ".hg"
            | ".svn"
            | "node_modules"
            | "__pycache__"
            | ".mypy_cache"
            | ".pytest_cache"
            | ".tox"
            | ".venv"
            | "venv"
            | ".env"
            | "env"
            | "target"
            | "dist"
            | "build"
            | ".next"
            | ".nuxt"
            | "vendor"
    ) || name.starts_with('.')
}

fn file_hash(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn file_modified(path: &Path) -> f64 {
    path.metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

// ── Symbol dedup ──

/// Disambiguate symbols with colliding stable IDs by appending `:N` suffixes.
///
/// When two symbols in the same file have the same `file:kind:qualified_name`
/// (e.g., conditional function definitions), the second occurrence gets `:2`, third `:3`, etc.
/// Edge source_ids and parent_ids are updated to match.
fn dedup_symbol_ids(symbols: &mut [Symbol], edges: &mut [cartog_core::Edge]) {
    use std::collections::{HashMap, HashSet};

    let mut seen: HashMap<String, u32> = HashMap::new();
    let mut renames: HashMap<String, String> = HashMap::new();

    for sym in symbols.iter_mut() {
        let count = seen.entry(sym.id.clone()).or_insert(0);
        *count += 1;
        if *count > 1 {
            let old_id = sym.id.clone();
            sym.id = format!("{old_id}:{count}");
            // First-rename wins: edges originally pointing at the collided id
            // (which the extractor produced without knowing which instance was
            // the owner) get attributed to the first renamed instance. The
            // zero-th instance keeps the short id and keeps its own edges only
            // if none collided — any ambiguity is resolved by sending the
            // ambiguous edges to the first-rename bucket, leaving the unrenamed
            // instance clean.
            renames.entry(old_id).or_insert_with(|| sym.id.clone());
        }
    }

    if !renames.is_empty() {
        for edge in edges.iter_mut() {
            if let Some(new_id) = renames.get(&edge.source_id) {
                edge.source_id = new_id.clone();
            }
        }

        for sym in symbols.iter_mut() {
            if let Some(ref pid) = sym.parent_id {
                if let Some(new_id) = renames.get(pid) {
                    sym.parent_id = Some(new_id.clone());
                }
            }
        }
    }

    // Invariant: after dedup, every edge.source_id must correspond to a
    // surviving symbol id. Broken invariants here cause foreign-key cascades
    // later and silent data loss, so bail loudly in debug builds.
    debug_assert!(
        {
            let ids: HashSet<&str> = symbols.iter().map(|s| s.id.as_str()).collect();
            edges.iter().all(|e| ids.contains(e.source_id.as_str()))
        },
        "dedup_symbol_ids left an edge with a dangling source_id"
    );
}

// ── Merkle-tree hashing ──

/// Compute content_hash and subtree_hash for all symbols in an extraction.
///
/// - content_hash = sha256(kind + name + signature + body_source)
/// - subtree_hash = sha256(content_hash + sorted(children_subtree_hashes))
///
/// Modifies symbols in-place.
fn compute_merkle_hashes(symbols: &mut [Symbol], source: &str) {
    use std::collections::HashMap;

    // Compute content_hash for each symbol
    for sym in symbols.iter_mut() {
        let body = source
            .get(sym.start_byte as usize..sym.end_byte as usize)
            .unwrap_or("");
        let mut hasher = Sha256::new();
        hasher.update(sym.kind.as_str().as_bytes());
        hasher.update(b":");
        hasher.update(sym.name.as_bytes());
        hasher.update(b":");
        if let Some(ref sig) = sym.signature {
            hasher.update(sig.as_bytes());
        }
        hasher.update(b":");
        hasher.update(body.as_bytes());
        sym.content_hash = Some(format!("{:x}", hasher.finalize()));
    }

    // Build parent→children map by index
    let id_to_idx: HashMap<&str, usize> = symbols
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id.as_str(), i))
        .collect();

    let mut children: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut roots: Vec<usize> = Vec::new();

    for (i, sym) in symbols.iter().enumerate() {
        if let Some(ref pid) = sym.parent_id {
            if let Some(&parent_idx) = id_to_idx.get(pid.as_str()) {
                children.entry(parent_idx).or_default().push(i);
            } else {
                roots.push(i);
            }
        } else {
            roots.push(i);
        }
    }

    // Post-order traversal to compute subtree_hash bottom-up
    let mut subtree_hashes: Vec<String> = vec![String::new(); symbols.len()];
    let mut stack: Vec<(usize, bool)> = roots.iter().rev().map(|&i| (i, false)).collect();

    while let Some((idx, visited)) = stack.pop() {
        if visited {
            // All children processed — compute subtree hash
            let mut hasher = Sha256::new();
            hasher.update(
                symbols[idx]
                    .content_hash
                    .as_deref()
                    .unwrap_or("")
                    .as_bytes(),
            );
            if let Some(kids) = children.get(&idx) {
                let mut kid_hashes: Vec<&str> =
                    kids.iter().map(|&k| subtree_hashes[k].as_str()).collect();
                kid_hashes.sort();
                for h in kid_hashes {
                    hasher.update(h.as_bytes());
                }
            }
            subtree_hashes[idx] = format!("{:x}", hasher.finalize());
        } else {
            stack.push((idx, true));
            if let Some(kids) = children.get(&idx) {
                for &kid in kids.iter().rev() {
                    stack.push((kid, false));
                }
            }
        }
    }

    // Store subtree_hash in symbols
    for (i, sym) in symbols.iter_mut().enumerate() {
        sym.subtree_hash = Some(std::mem::take(&mut subtree_hashes[i]));
    }
}

/// Result of diffing new symbols against stored hashes.
#[derive(Debug, Default)]
struct SymbolDiff {
    added: Vec<usize>,            // indices into new symbols
    removed: Vec<String>,         // IDs to delete from DB
    modified: Vec<usize>,         // indices into new symbols (content changed)
    children_changed: Vec<usize>, // indices: own content same, child subtree differs
    unchanged: usize,             // count of fully unchanged symbols
}

/// Compare newly extracted symbols against stored hashes for a file.
fn merkle_diff(
    new_symbols: &[Symbol],
    old_hashes: &[(String, Option<String>, Option<String>)],
) -> SymbolDiff {
    use std::collections::{HashMap, HashSet};

    let mut diff = SymbolDiff::default();

    let old_map: HashMap<&str, (&Option<String>, &Option<String>)> = old_hashes
        .iter()
        .map(|(id, ch, sh)| (id.as_str(), (ch, sh)))
        .collect();

    let new_ids: HashSet<&str> = new_symbols.iter().map(|s| s.id.as_str()).collect();

    for (i, sym) in new_symbols.iter().enumerate() {
        if let Some(&(old_ch, old_sh)) = old_map.get(sym.id.as_str()) {
            // Symbol exists in both old and new
            if sym.subtree_hash.as_ref() == old_sh.as_ref()
                && sym.content_hash.as_ref() == old_ch.as_ref()
            {
                diff.unchanged += 1;
            } else if sym.content_hash.as_ref() != old_ch.as_ref() {
                diff.modified.push(i);
            } else {
                // content same, subtree different — a child was added/modified/removed
                diff.children_changed.push(i);
            }
        } else {
            diff.added.push(i);
        }
    }

    for (old_id, _, _) in old_hashes {
        if !new_ids.contains(old_id.as_str()) {
            diff.removed.push(old_id.clone());
        }
    }

    diff
}

/// Get list of files changed since the last indexed commit.
///
/// Returns `None` (triggering hash fallback) when:
/// - `last_commit` is `None` (first index)
/// - Not inside a git repository
/// - The stored commit no longer exists (after rebase/reset)
fn git_changed_files(
    root: &Path,
    last_commit: Option<&str>,
) -> Option<std::collections::HashSet<String>> {
    let last_commit = last_commit?;

    // Verify the stored commit still exists in history
    let verify = git_cmd(root, &["cat-file", "-t", last_commit])?;
    if !verify.status.success() {
        return None;
    }

    // Get files changed between last indexed commit and HEAD
    let diff_output = git_cmd(root, &["diff", "--name-only", last_commit, "HEAD"])?;
    if !diff_output.status.success() {
        return None;
    }

    let mut changed: std::collections::HashSet<String> =
        parse_git_lines(&diff_output.stdout).collect();

    // Also include untracked files (new files not yet committed)
    if let Some(out) = git_cmd(root, &["ls-files", "--others", "--exclude-standard"]) {
        if out.status.success() {
            changed.extend(parse_git_lines(&out.stdout));
        }
    }

    // Also include unstaged/staged changes in the working tree
    if let Some(out) = git_cmd(root, &["diff", "--name-only"]) {
        if out.status.success() {
            changed.extend(parse_git_lines(&out.stdout));
        }
    }

    if let Some(out) = git_cmd(root, &["diff", "--name-only", "--cached"]) {
        if out.status.success() {
            changed.extend(parse_git_lines(&out.stdout));
        }
    }

    Some(changed)
}

/// Get the current HEAD commit hash.
fn git_head_commit(root: &Path) -> Option<String> {
    let output = git_cmd(root, &["rev-parse", "HEAD"])?;
    if output.status.success() {
        Some(String::from_utf8(output.stdout).ok()?.trim().to_string())
    } else {
        None
    }
}

/// Get files changed in the last N commits + working tree changes (staged, unstaged, untracked).
///
/// Returns a sorted, deduplicated list of file paths relative to `root`.
/// Returns `Err` if not inside a git repository.
pub fn git_recently_changed_files(root: &Path, commits: u32) -> Result<Vec<String>> {
    use std::collections::BTreeSet;
    let mut changed = BTreeSet::new();

    // Files changed in last N commits
    let output = git_cmd(
        root,
        &[
            "log",
            "--name-only",
            "--pretty=format:",
            &format!("-{commits}"),
        ],
    )
    .context("Failed to run git — are you in a git repository?")?;
    if output.status.success() {
        changed.extend(parse_git_lines(&output.stdout));
    }

    // Working tree changes (unstaged + staged + untracked)
    for args in [
        &["diff", "--name-only"][..],
        &["diff", "--name-only", "--cached"][..],
        &["ls-files", "--others", "--exclude-standard"][..],
    ] {
        if let Some(out) = git_cmd(root, args) {
            if out.status.success() {
                changed.extend(parse_git_lines(&out.stdout));
            }
        }
    }

    Ok(changed.into_iter().collect())
}

/// Run a git command with stdin suppressed to prevent interactive prompts.
fn git_cmd(root: &Path, args: &[&str]) -> Option<std::process::Output> {
    std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()
}

/// Parse lines from git command output, filtering empty lines.
fn parse_git_lines(stdout: &[u8]) -> impl Iterator<Item = String> + '_ {
    String::from_utf8_lossy(stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .into_iter()
}

/// Find the largest byte index <= `index` that is a valid UTF-8 char boundary in `s`.
///
/// Equivalent to the nightly `str::floor_char_boundary`. Walks back at most 3 bytes.
fn floor_char_boundary(s: &str, index: usize) -> usize {
    if index >= s.len() {
        return s.len();
    }
    // UTF-8 continuation bytes have the pattern 10xxxxxx (0x80..0xBF).
    // Walk backwards until we find a byte that is NOT a continuation byte.
    let mut i = index;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Maximum content length (in bytes) stored per symbol for embedding.
///
/// BERT models have a 512-token limit. Code averages ~1 token per 2-3 chars,
/// so 2048 bytes ≈ 680-1024 tokens, truncated to 512 by the model.
/// This captures the symbol signature + leading body while halving inference time.
const MAX_CONTENT_BYTES: usize = 2048;

/// Minimum content length (in bytes) to bother embedding.
///
/// Symbols shorter than this (e.g. `import os`, `x = 1`) add noise without value.
const MIN_CONTENT_BYTES: usize = 50;

/// Extract the raw source code for a symbol and build a metadata header.
///
/// Returns `(content, header)` where `header` is a brief preamble for embedding context.
/// Returns `None` if: byte offsets are invalid, content is empty/too short,
/// or the symbol is an import (not useful for semantic search).
fn extract_symbol_content(source: &str, sym: &cartog_core::Symbol) -> Option<(String, String)> {
    // Skip imports — they don't contain searchable logic.
    if sym.kind == cartog_core::SymbolKind::Import {
        return None;
    }

    let start = sym.start_byte as usize;
    let end = sym.end_byte as usize;

    if start >= end || end > source.len() {
        return None;
    }

    // Ensure both boundaries fall on valid UTF-8 char boundaries.
    // Tree-sitter should produce valid offsets, but truncation at MAX_CONTENT_BYTES
    // can land mid-character for multi-byte content (e.g. '─' = 3 bytes).
    let safe_start = if source.is_char_boundary(start) {
        start
    } else {
        // Ceil to next char boundary
        let mut s = start;
        while s < source.len() && !source.is_char_boundary(s) {
            s += 1;
        }
        s
    };
    let truncated_end = end.min(safe_start + MAX_CONTENT_BYTES);
    let safe_end = floor_char_boundary(source, truncated_end);

    if safe_start >= safe_end {
        return None;
    }

    let raw = &source[safe_start..safe_end];
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() < MIN_CONTENT_BYTES {
        return None;
    }

    let header = format!(
        "// File: {}\n// Type: {}\n// Name: {}",
        sym.file_path, sym.kind, sym.name
    );

    Some((raw.to_string(), header))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_hash_deterministic() {
        let h1 = file_hash("def foo(): pass");
        let h2 = file_hash("def foo(): pass");
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_file_hash_different_content() {
        let h1 = file_hash("def foo(): pass");
        let h2 = file_hash("def bar(): pass");
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_is_ignored_directories() {
        let tmp = std::env::temp_dir().join("cartog_test_ignored");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let ignored_dirs = [
            ".git",
            "node_modules",
            "__pycache__",
            "target",
            "dist",
            "build",
            ".venv",
        ];
        let allowed_dirs = ["src", "lib", "tests", "docs"];

        for name in ignored_dirs.iter().chain(allowed_dirs.iter()) {
            std::fs::create_dir_all(tmp.join(name)).unwrap();
        }

        let entries: Vec<_> = WalkDir::new(&tmp)
            .min_depth(1)
            .max_depth(1)
            .into_iter()
            .filter_map(|e| e.ok())
            .collect();

        for entry in &entries {
            let name = entry.file_name().to_string_lossy();
            if ignored_dirs.contains(&name.as_ref()) {
                assert!(is_ignored(entry), "{name} should be ignored");
            }
            if allowed_dirs.contains(&name.as_ref()) {
                assert!(!is_ignored(entry), "{name} should NOT be ignored");
            }
        }

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_git_changed_files_no_commit() {
        // When last_commit is None, should return None (first index → full scan)
        let result = git_changed_files(Path::new("."), None);
        assert!(result.is_none());
    }

    #[test]
    fn test_git_changed_files_invalid_commit() {
        // A commit hash that doesn't exist should return None (fallback to hash)
        let result = git_changed_files(
            Path::new("."),
            Some("0000000000000000000000000000000000000000"),
        );
        assert!(result.is_none());
    }

    #[test]
    fn test_git_changed_files_valid_head() {
        // If we diff HEAD against HEAD, the changed set should be empty
        // (only working tree / untracked files would appear)
        let head = git_head_commit(Path::new("."));
        if let Some(commit) = head {
            let result = git_changed_files(Path::new("."), Some(&commit));
            // Should return Some (valid commit), though the set may contain untracked/modified files
            assert!(result.is_some());
        }
    }

    #[test]
    fn test_index_directory_force() {
        use cartog_db::Database;

        let db = Database::open_memory().unwrap();
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/auth");

        if fixtures.exists() {
            // First index
            let r1 = index_directory(&db, &fixtures, false, false).unwrap();
            assert!(r1.files_indexed > 0);

            // Second index without force — should skip all files
            let r2 = index_directory(&db, &fixtures, false, false).unwrap();
            assert_eq!(r2.files_indexed, 0);
            assert!(r2.files_skipped > 0);

            // Force re-index — should re-index all files
            let r3 = index_directory(&db, &fixtures, true, false).unwrap();
            assert_eq!(r3.files_indexed, r1.files_indexed);
            assert_eq!(r3.files_skipped, 0);
        }
    }

    #[test]
    fn test_floor_char_boundary_ascii() {
        let s = "hello world";
        assert_eq!(floor_char_boundary(s, 5), 5);
        assert_eq!(floor_char_boundary(s, 0), 0);
        assert_eq!(floor_char_boundary(s, 100), s.len());
    }

    #[test]
    fn test_floor_char_boundary_multibyte() {
        // '─' is U+2500, encoded as 3 bytes: E2 94 80
        let s = "abc─def";
        // a=0, b=1, c=2, ─=3..6, d=6, e=7, f=8
        assert_eq!(floor_char_boundary(s, 3), 3); // start of ─
        assert_eq!(floor_char_boundary(s, 4), 3); // mid ─ → snap back
        assert_eq!(floor_char_boundary(s, 5), 3); // mid ─ → snap back
        assert_eq!(floor_char_boundary(s, 6), 6); // start of 'd'
    }

    #[test]
    fn test_extract_symbol_content_truncates_at_char_boundary() {
        // Build a source string where MAX_CONTENT_BYTES truncation lands mid-char.
        // Fill with ASCII up to MAX_CONTENT_BYTES-1, then add a 3-byte char.
        let padding = "x".repeat(MAX_CONTENT_BYTES - 1);
        let source = format!("{padding}─after");

        let sym = cartog_core::Symbol::new(
            "test_sym",
            cartog_core::SymbolKind::Function,
            "test.rb",
            1,
            100,
            0,
            source.len() as u32,
            None,
        );

        // This should NOT panic despite truncation landing inside '─'
        let result = extract_symbol_content(&source, &sym);
        assert!(result.is_some());
        let (content, _header) = result.unwrap();
        // Content should be truncated before the '─' (snapped to char boundary)
        assert_eq!(content.len(), MAX_CONTENT_BYTES - 1);
        assert!(content.is_char_boundary(content.len()));
    }

    // ── Dedup tests ──

    #[test]
    fn test_dedup_3way_collision_preserves_invariant() {
        // Three symbols with the same stable id — simulates conditional
        // redefinitions (e.g. `if/elif/else: def foo`).
        let mk_sym = || {
            cartog_core::Symbol::new(
                "foo",
                cartog_core::SymbolKind::Function,
                "test.py",
                1,
                2,
                0,
                10,
                None,
            )
        };
        let base_id = mk_sym().id.clone();
        let mut symbols = vec![mk_sym(), mk_sym(), mk_sym()];
        let mut edges = vec![
            cartog_core::Edge::new(
                base_id.clone(),
                "bar",
                cartog_core::EdgeKind::Calls,
                "test.py",
                1,
            ),
            cartog_core::Edge::new(
                base_id.clone(),
                "baz",
                cartog_core::EdgeKind::Calls,
                "test.py",
                2,
            ),
        ];

        dedup_symbol_ids(&mut symbols, &mut edges);

        // All three ids must now be distinct.
        let ids: std::collections::HashSet<_> = symbols.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids.len(), 3, "3-way collision should produce 3 unique ids");

        // First instance keeps the short id; 2nd and 3rd get numeric suffixes.
        assert_eq!(symbols[0].id, base_id);
        assert_eq!(symbols[1].id, format!("{base_id}:2"));
        assert_eq!(symbols[2].id, format!("{base_id}:3"));

        // Invariant: every edge.source_id must resolve to a surviving symbol.
        for edge in &edges {
            assert!(
                ids.contains(edge.source_id.as_str()),
                "edge source_id {:?} has no matching symbol after dedup",
                edge.source_id
            );
        }
    }

    #[test]
    fn test_dedup_no_collision_leaves_ids_unchanged() {
        let mut symbols = vec![
            cartog_core::Symbol::new(
                "a",
                cartog_core::SymbolKind::Function,
                "f.py",
                1,
                2,
                0,
                10,
                None,
            ),
            cartog_core::Symbol::new(
                "b",
                cartog_core::SymbolKind::Function,
                "f.py",
                3,
                4,
                11,
                20,
                None,
            ),
        ];
        let id_a = symbols[0].id.clone();
        let id_b = symbols[1].id.clone();
        let mut edges: Vec<cartog_core::Edge> = vec![];
        dedup_symbol_ids(&mut symbols, &mut edges);
        assert_eq!(symbols[0].id, id_a);
        assert_eq!(symbols[1].id, id_b);
    }

    // ── Merkle hashing tests ──

    #[test]
    fn test_compute_merkle_hashes_populates_fields() {
        let source = "def foo():\n    pass\n";
        let mut symbols = vec![cartog_core::Symbol::new(
            "foo",
            cartog_core::SymbolKind::Function,
            "test.py",
            1,
            2,
            0,
            source.len() as u32,
            None,
        )];

        compute_merkle_hashes(&mut symbols, source);

        assert!(symbols[0].content_hash.is_some());
        assert!(symbols[0].subtree_hash.is_some());
    }

    #[test]
    fn test_merkle_hashes_stable_across_position_changes() {
        let source_v1 = "def foo():\n    pass\n";
        let source_v2 = "\n\ndef foo():\n    pass\n";

        let mut sym_v1 = vec![cartog_core::Symbol::new(
            "foo",
            cartog_core::SymbolKind::Function,
            "test.py",
            1,
            2,
            0,
            source_v1.len() as u32,
            None,
        )];
        let mut sym_v2 = vec![cartog_core::Symbol::new(
            "foo",
            cartog_core::SymbolKind::Function,
            "test.py",
            3,
            4,
            2,
            source_v2.len() as u32,
            None,
        )];

        compute_merkle_hashes(&mut sym_v1, source_v1);
        compute_merkle_hashes(&mut sym_v2, source_v2);

        // content_hash depends on body text — different offset means different body slice
        // but if the body text is the same, hashes should match
        // Here the body text is the same "def foo():\n    pass\n"
        assert_eq!(sym_v1[0].content_hash, sym_v2[0].content_hash);
    }

    #[test]
    fn test_merkle_diff_detects_added_symbol() {
        let old_hashes: Vec<(String, Option<String>, Option<String>)> = vec![];

        let mut new_symbols = vec![cartog_core::Symbol::new(
            "foo",
            cartog_core::SymbolKind::Function,
            "test.py",
            1,
            5,
            0,
            50,
            None,
        )];
        new_symbols[0].content_hash = Some("abc".to_string());
        new_symbols[0].subtree_hash = Some("def".to_string());

        let diff = merkle_diff(&new_symbols, &old_hashes);
        assert_eq!(diff.added.len(), 1);
        assert_eq!(diff.removed.len(), 0);
        assert_eq!(diff.modified.len(), 0);
    }

    #[test]
    fn test_merkle_diff_detects_removed_symbol() {
        let old_hashes = vec![(
            "test.py:function:foo".to_string(),
            Some("abc".to_string()),
            Some("def".to_string()),
        )];

        let new_symbols: Vec<cartog_core::Symbol> = vec![];

        let diff = merkle_diff(&new_symbols, &old_hashes);
        assert_eq!(diff.added.len(), 0);
        assert_eq!(diff.removed.len(), 1);
        assert_eq!(diff.removed[0], "test.py:function:foo");
    }

    #[test]
    fn test_merkle_diff_detects_unchanged() {
        let old_hashes = vec![(
            "test.py:function:foo".to_string(),
            Some("abc".to_string()),
            Some("def".to_string()),
        )];

        let mut new_symbols = vec![cartog_core::Symbol::new(
            "foo",
            cartog_core::SymbolKind::Function,
            "test.py",
            1,
            5,
            0,
            50,
            None,
        )];
        new_symbols[0].content_hash = Some("abc".to_string());
        new_symbols[0].subtree_hash = Some("def".to_string());

        let diff = merkle_diff(&new_symbols, &old_hashes);
        assert_eq!(diff.unchanged, 1);
        assert_eq!(diff.added.len(), 0);
        assert_eq!(diff.modified.len(), 0);
    }

    #[test]
    fn test_merkle_diff_detects_modified() {
        let old_hashes = vec![(
            "test.py:function:foo".to_string(),
            Some("old_hash".to_string()),
            Some("old_subtree".to_string()),
        )];

        let mut new_symbols = vec![cartog_core::Symbol::new(
            "foo",
            cartog_core::SymbolKind::Function,
            "test.py",
            1,
            5,
            0,
            50,
            None,
        )];
        new_symbols[0].content_hash = Some("new_hash".to_string());
        new_symbols[0].subtree_hash = Some("new_subtree".to_string());

        let diff = merkle_diff(&new_symbols, &old_hashes);
        assert_eq!(diff.modified.len(), 1);
        assert_eq!(diff.unchanged, 0);
    }

    // ── Integration test: full incremental pipeline ──

    #[test]
    fn test_incremental_merkle_diff_pipeline() {
        use cartog_db::Database;

        let tmp = tempfile::TempDir::new().unwrap();
        // Create a non-dot subdirectory (tempfile may create .tmpXXX on macOS,
        // which is_ignored_dirname skips)
        let dir = tmp.path().join("project");
        std::fs::create_dir(&dir).unwrap();

        // Initial files
        let a_py = dir.join("a.py");
        let b_py = dir.join("b.py");

        std::fs::write(
            &a_py,
            r#"class Greeter:
    def hello(self):
        return "hi"
    def goodbye(self):
        return "bye"
"#,
        )
        .unwrap();

        std::fs::write(
            &b_py,
            r#"from a import Greeter
def main():
    g = Greeter()
    g.hello()
"#,
        )
        .unwrap();

        let db = Database::open_memory().unwrap();

        // ── Index 1: initial full index ──
        let r1 = index_directory(&db, &dir, true, false).unwrap();
        assert_eq!(r1.files_indexed, 2);
        assert!(r1.symbols_added > 0, "should have symbols");

        let outline_a = db.outline("a.py").unwrap();
        assert_eq!(outline_a.len(), 3, "Greeter + hello + goodbye");
        let names_a: Vec<&str> = outline_a.iter().map(|s| s.name.as_str()).collect();
        assert!(names_a.contains(&"Greeter"));
        assert!(names_a.contains(&"hello"));
        assert!(names_a.contains(&"goodbye"));

        // Capture stable IDs
        let hello_id_v1 = outline_a
            .iter()
            .find(|s| s.name == "hello")
            .unwrap()
            .id
            .clone();
        let greeter_id_v1 = outline_a
            .iter()
            .find(|s| s.name == "Greeter")
            .unwrap()
            .id
            .clone();

        // Verify Merkle hashes populated
        let hashes = db.get_symbol_hashes_for_file("a.py").unwrap();
        assert!(
            hashes
                .iter()
                .all(|(_, ch, sh)| ch.is_some() && sh.is_some()),
            "all symbols should have hashes after indexing"
        );

        // ── Index 2: add a function to a.py ──
        std::fs::write(
            &a_py,
            r#"class Greeter:
    def hello(self):
        return "hi"
    def goodbye(self):
        return "bye"

def standalone():
    return "I am new"
"#,
        )
        .unwrap();

        let r2 = index_directory(&db, &dir, false, false).unwrap();
        assert_eq!(r2.files_indexed, 1, "only a.py changed");
        assert!(r2.files_skipped > 0, "b.py should be skipped");
        assert_eq!(r2.symbols_added, 1, "standalone is new");
        assert!(
            r2.symbols_unchanged >= 2,
            "hello and goodbye should be unchanged, got {}",
            r2.symbols_unchanged
        );

        let outline_a2 = db.outline("a.py").unwrap();
        assert_eq!(
            outline_a2.len(),
            4,
            "Greeter + hello + goodbye + standalone"
        );
        assert!(outline_a2.iter().any(|s| s.name == "standalone"));

        // Verify ID stability: hello and Greeter keep same IDs
        let hello_id_v2 = outline_a2
            .iter()
            .find(|s| s.name == "hello")
            .unwrap()
            .id
            .clone();
        let greeter_id_v2 = outline_a2
            .iter()
            .find(|s| s.name == "Greeter")
            .unwrap()
            .id
            .clone();
        assert_eq!(hello_id_v1, hello_id_v2, "hello ID should be stable");
        assert_eq!(greeter_id_v1, greeter_id_v2, "Greeter ID should be stable");

        // ── Index 3: remove goodbye from a.py ──
        std::fs::write(
            &a_py,
            r#"class Greeter:
    def hello(self):
        return "hi"

def standalone():
    return "I am new"
"#,
        )
        .unwrap();

        let r3 = index_directory(&db, &dir, false, false).unwrap();
        assert_eq!(r3.files_indexed, 1);
        assert!(r3.symbols_removed >= 1, "goodbye should be removed");

        let outline_a3 = db.outline("a.py").unwrap();
        assert_eq!(outline_a3.len(), 3, "Greeter + hello + standalone");
        assert!(
            !outline_a3.iter().any(|s| s.name == "goodbye"),
            "goodbye should be gone"
        );

        // hello ID still stable after removal of sibling
        let hello_id_v3 = outline_a3
            .iter()
            .find(|s| s.name == "hello")
            .unwrap()
            .id
            .clone();
        assert_eq!(
            hello_id_v1, hello_id_v3,
            "hello ID stable after sibling removal"
        );
    }

    // ── Integration test: Markdown document indexing ──

    #[test]
    fn test_markdown_indexing_end_to_end() {
        use cartog_db::Database;

        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("project");
        std::fs::create_dir(&dir).unwrap();

        let md_file = dir.join("design.md");
        std::fs::write(
            &md_file,
            r#"# Architecture

This document describes the system architecture.

## Authentication

Users authenticate via JWT tokens. The server validates
the token signature and checks expiration before granting access.

## Database

We use PostgreSQL with connection pooling via pgbouncer.
"#,
        )
        .unwrap();

        let db = Database::open_memory().unwrap();
        let result = index_directory(&db, &dir, false, false).unwrap();

        assert_eq!(result.files_indexed, 1);
        assert!(result.symbols_added >= 3, "should have at least 3 sections");

        // Verify file entry
        let file = db.get_file("design.md").unwrap();
        assert!(file.is_some());
        let file = file.unwrap();
        assert_eq!(file.language, "markdown");

        // Verify Document symbols exist
        let outline = db.outline("design.md").unwrap();
        assert!(
            outline.len() >= 3,
            "should have Architecture, Authentication, Database sections"
        );

        let names: Vec<&str> = outline.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names.contains(&"Architecture"),
            "missing Architecture section"
        );
        assert!(
            names.contains(&"Authentication"),
            "missing Authentication section"
        );
        assert!(names.contains(&"Database"), "missing Database section");

        for sym in &outline {
            assert_eq!(sym.kind, cartog_core::SymbolKind::Document);
        }

        // Verify symbol_content is populated
        let auth_sym = outline.iter().find(|s| s.name == "Authentication").unwrap();
        let content = db.get_symbol_content(&auth_sym.id).unwrap();
        assert!(
            content.is_some(),
            "symbol_content should exist for document section"
        );
        let (text, header) = content.unwrap();
        assert!(
            text.contains("JWT tokens"),
            "content should include section body"
        );
        assert!(
            header.contains("Authentication"),
            "header should include section name"
        );
    }
}
