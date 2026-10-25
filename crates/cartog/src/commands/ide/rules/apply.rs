//! One rule file to one report step: write or refresh it on install, delete it on
//! removal. Only a file holding cartog's marker is ever rewritten or deleted.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use super::catalogue::{blocker, Blocker, RuleTarget};
use super::content::{is_cartog_owned, render};
use crate::cli::ClientKind;
use crate::commands::ide::catalogue::client_name;
use crate::commands::ide::run::atomic_write;
use crate::commands::ide::{DiffPair, FileKind, IdeStatus, IdeStep};

/// Write `target`'s rule file, refresh a stale cartog copy, or report why it was left alone.
pub fn install_rule(target: &RuleTarget, cwd: &Path, dry_run: bool) -> IdeStep {
    let make = |status, message: String, diff| step(target, status, message, diff);
    if let Some(blocked) = blocker(target.kind, cwd) {
        return make(IdeStatus::Skipped, blocker_message(&blocked, cwd), None);
    }
    let existing = match read(target) {
        Ok(existing) => existing,
        Err(message) => return make(IdeStatus::Error, message, None),
    };
    let rendered = render(&target.format);
    let (status, planned, done) = match existing.as_deref() {
        None => (IdeStatus::Created, "would create", "created"),
        Some(text) if !is_cartog_owned(text) => {
            return make(
                IdeStatus::Skipped,
                "not written by cartog (no marker line); left as is".into(),
                None,
            );
        }
        // A checkout with `core.autocrlf` must not look stale on every run.
        Some(text) if text.replace("\r\n", "\n") == rendered => {
            return make(IdeStatus::Unchanged, "already up to date".into(), None);
        }
        Some(_) => (IdeStatus::Updated, "would update", "updated"),
    };
    if dry_run {
        let diff = DiffPair {
            before: existing,
            after: rendered,
        };
        return make(status, planned.into(), Some(diff));
    }
    match atomic_write(&target.path, &rendered) {
        Ok(()) => make(status, done.into(), None),
        Err(e) => make(IdeStatus::Error, format!("write failed: {e:#}"), None),
    }
}

/// Delete `target`'s rule file when cartog owns it. A symlink loses the link, not its target.
pub fn remove_rule(target: &RuleTarget, dry_run: bool) -> IdeStep {
    let make = |status, message: String, diff| step(target, status, message, diff);
    let content = match read(target) {
        Ok(Some(content)) => content,
        Ok(None) => {
            return make(
                IdeStatus::Unchanged,
                "not wired (no rule file)".into(),
                None,
            )
        }
        Err(message) => return make(IdeStatus::Error, message, None),
    };
    if !is_cartog_owned(&content) {
        return make(
            IdeStatus::Skipped,
            "not written by cartog; left in place".into(),
            None,
        );
    }
    if dry_run {
        let diff = DiffPair {
            before: Some(content),
            after: String::new(),
        };
        return make(IdeStatus::Removed, "would remove".into(), Some(diff));
    }
    match fs::remove_file(&target.path) {
        Ok(()) => make(IdeStatus::Removed, "removed".into(), None),
        Err(e) => make(
            IdeStatus::Error,
            format!("could not delete {}: {e}", target.path.display()),
            None,
        ),
    }
}

// Users run the binary outside the cartog repo, so a repo-relative docs path would not resolve.
const SNIPPET_URL: &str = "https://github.com/jrollin/cartog/blob/main/docs/agent-snippet.md";

/// Step for a client named with `--rules` that has no rules directory to write to.
pub fn unsupported_rule_step(kind: ClientKind) -> IdeStep {
    IdeStep {
        client: client_name(kind).to_string(),
        scope: "project".into(),
        path: String::new(),
        file_kind: FileKind::Rule,
        status: IdeStatus::Skipped,
        message: format!(
            "no rules directory for this client; add the snippet from {SNIPPET_URL} \
             to its rules file by hand"
        ),
        diff: None,
    }
}

fn step(
    target: &RuleTarget,
    status: IdeStatus,
    message: String,
    diff: Option<DiffPair>,
) -> IdeStep {
    IdeStep {
        client: client_name(target.kind).to_string(),
        scope: "project".into(),
        path: target.path.display().to_string(),
        file_kind: FileKind::Rule,
        status,
        message,
        diff,
    }
}

/// The rule file's content, `None` when absent, or an error message naming the path.
/// Invalid UTF-8 is decoded lossily: such a file only counts as cartog's if the marker survives.
fn read(target: &RuleTarget) -> Result<Option<String>, String> {
    match fs::read(&target.path) {
        Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
        // A legacy `.clinerules` file in place of the rules dir: there is no rule file.
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => Ok(None),
        Err(e) => Err(format!(
            "could not read rule file {}: {e}",
            target.path.display()
        )),
    }
}

fn blocker_message(blocked: &Blocker, cwd: &Path) -> String {
    let rel = |path: &Path| path.strip_prefix(cwd).unwrap_or(path).display().to_string();
    match blocked {
        Blocker::FallbackFile(file) => format!(
            "{} is in use and Roo Code stops reading it once .roo/rules/ has a file; \
             copy its content into a file under .roo/rules/ and re-run",
            rel(file)
        ),
        Blocker::NotADirectory(file) => format!(
            "{} is a file, not a directory, so cartog cannot add its rule; \
             add the snippet from {SNIPPET_URL} to the agent's rules by hand",
            rel(file)
        ),
    }
}
