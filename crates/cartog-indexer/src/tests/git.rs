//! Tests for git-changed-file detection.

use crate::*;

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
