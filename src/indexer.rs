use std::path::Path;
use std::time::SystemTime;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use tracing::warn;
use walkdir::WalkDir;

use crate::db::Database;
use crate::languages::{detect_language, get_extractor};
use crate::types::FileInfo;

/// Summary of an indexing operation.
#[derive(Debug, Default, serde::Serialize)]
pub struct IndexResult {
    pub files_indexed: u32,
    pub files_skipped: u32,
    pub files_removed: u32,
    pub symbols_added: u32,
    pub edges_added: u32,
    pub edges_resolved: u32,
}

/// Index a directory, updating the database incrementally.
///
/// Change detection strategy (in order):
/// 1. `force = true` → re-index everything, no checks
/// 2. Git-based → diff `last_commit..HEAD` to find changed files, skip the rest without reading
/// 3. SHA-256 fallback → read file, hash it, compare to stored hash
pub fn index_directory(db: &Database, root: &Path, force: bool) -> Result<IndexResult> {
    let mut result = IndexResult::default();

    let root = root.canonicalize().context("Failed to resolve root path")?;

    // Collect files that should be indexed
    let mut current_files = std::collections::HashSet::new();

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

    for entry in WalkDir::new(&root)
        .follow_links(true)
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

        // ── Change detection (deferred file read) ──
        if !force {
            if let Some(ref changed) = changed_files {
                // Git-based: skip files not in the changed set that already exist in db
                if !changed.contains(&rel_path) && db.get_file(&rel_path)?.is_some() {
                    result.files_skipped += 1;
                    continue;
                }
            }
        }

        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => continue, // binary file
            Err(e) => {
                warn!(file = %rel_path, error = %e, "cannot read file");
                continue;
            }
        };

        let hash = file_hash(&source);

        // Hash-based check: even for git-detected changes, skip if content is identical
        // (handles touched-but-not-modified files)
        if !force {
            if let Ok(Some(existing)) = db.get_file(&rel_path) {
                if existing.hash == hash {
                    result.files_skipped += 1;
                    continue;
                }
            }
        }

        let modified = file_modified(path);

        // Extract symbols and edges
        let extractor = match get_extractor(lang) {
            Some(e) => e,
            None => {
                result.files_skipped += 1;
                continue;
            }
        };

        let extraction = match extractor.extract(&source, &rel_path) {
            Ok(e) => e,
            Err(err) => {
                warn!(file = %rel_path, error = %err, "extraction failed");
                continue;
            }
        };

        // Clear old data and insert new
        db.clear_file_data(&rel_path)?;

        let num_symbols = extraction.symbols.len() as u32;
        let num_edges = extraction.edges.len() as u32;

        db.insert_symbols(&extraction.symbols)?;
        db.insert_edges(&extraction.edges)?;

        db.upsert_file(&FileInfo {
            path: rel_path,
            last_modified: modified,
            hash,
            language: lang.to_string(),
            num_symbols,
        })?;

        result.files_indexed += 1;
        result.symbols_added += num_symbols;
        result.edges_added += num_edges;
    }

    // Remove files that no longer exist
    let all_indexed = db.all_files()?;
    for indexed_path in all_indexed {
        if !current_files.contains(&indexed_path) {
            db.remove_file(&indexed_path)?;
            result.files_removed += 1;
        }
    }

    // Resolve edges
    result.edges_resolved = db.resolve_edges()?;

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
        return matches!(
            name.as_ref(),
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
        ) || name.starts_with('.');
    }

    false
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
        use crate::db::Database;

        let db = Database::open_memory().unwrap();
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/auth");

        if fixtures.exists() {
            // First index
            let r1 = index_directory(&db, &fixtures, false).unwrap();
            assert!(r1.files_indexed > 0);

            // Second index without force — should skip all files
            let r2 = index_directory(&db, &fixtures, false).unwrap();
            assert_eq!(r2.files_indexed, 0);
            assert!(r2.files_skipped > 0);

            // Force re-index — should re-index all files
            let r3 = index_directory(&db, &fixtures, true).unwrap();
            assert_eq!(r3.files_indexed, r1.files_indexed);
            assert_eq!(r3.files_skipped, 0);
        }
    }
}
