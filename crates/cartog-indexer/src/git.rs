//! Git integration: changed-files detection and recent-commit queries.

use super::*;

/// Get list of files changed since the last indexed commit.
///
/// Returns `None` (triggering hash fallback) when:
/// - `last_commit` is `None` (first index)
/// - Not inside a git repository
/// - The stored commit no longer exists (after rebase/reset)
pub(crate) fn git_changed_files(
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
pub(crate) fn git_head_commit(root: &Path) -> Option<String> {
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
