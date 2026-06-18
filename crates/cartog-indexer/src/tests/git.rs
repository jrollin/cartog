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
    // Diffing the working tree against the current HEAD: the result is Some
    // for a valid commit (the set may include uncommitted/untracked files).
    // expect() so the test fails loudly outside a git checkout rather than
    // silently skipping the assertion below.
    let head = git_head_commit(Path::new(".")).expect("test runs inside a git repo");
    let result = git_changed_files(Path::new("."), Some(&head));
    assert!(result.is_some());
}
