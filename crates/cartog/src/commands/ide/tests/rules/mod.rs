//! Rule file tests, split by module; shared fixtures for a temp repo and home.

use std::fs;
use std::path::{Path, PathBuf};

use crate::commands::ide::catalogue::HomeDirs;

mod apply;
mod catalogue;
mod content;
mod plan;

/// A temp dir holding a repo (the working directory) and a separate home.
pub(super) struct Sandbox {
    _tmp: tempfile::TempDir,
    pub(super) cwd: PathBuf,
    pub(super) home: PathBuf,
}

impl Sandbox {
    pub(super) fn new() -> Self {
        let tmp = tempfile::TempDir::new().expect("temp dir");
        let cwd = tmp.path().join("repo");
        let home = tmp.path().join("home");
        fs::create_dir_all(&cwd).expect("create repo dir");
        fs::create_dir_all(&home).expect("create home dir");
        Self {
            _tmp: tmp,
            cwd,
            home,
        }
    }

    /// User config locations of the 8 rule agents, under the sandbox home.
    pub(super) fn homes(&self) -> HomeDirs {
        let h = &self.home;
        let ext = h.join("Code/User/globalStorage");
        HomeDirs {
            claude_code: h.join(".claude/settings.json"),
            cursor: h.join(".cursor/mcp.json"),
            cursor_app: h.join("Library/Cursor"),
            windsurf: h.join(".codeium/windsurf/mcp_config.json"),
            kiro: h.join(".kiro/settings/mcp.json"),
            antigravity: h.join(".gemini/config/mcp_config.json"),
            vscode: h.join("Code/User/mcp.json"),
            cline: ext.join("saoudrizwan.claude-dev/settings/cline_mcp_settings.json"),
            roo: ext.join("rooveterinaryinc.roo-cline/settings/mcp_settings.json"),
            ..HomeDirs::default()
        }
    }

    /// Create `rel` (and its parents) as a directory under the repo.
    pub(super) fn dir(&self, rel: &str) -> PathBuf {
        let path = self.cwd.join(rel);
        fs::create_dir_all(&path).expect("create dir");
        path
    }

    /// Write `content` to `rel` under the repo, creating parents.
    pub(super) fn file(&self, rel: &str, content: &str) -> PathBuf {
        write(&self.cwd.join(rel), content)
    }
}

pub(super) fn write(path: &Path, content: &str) -> PathBuf {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent dir");
    }
    fs::write(path, content).expect("write file");
    path.to_path_buf()
}
