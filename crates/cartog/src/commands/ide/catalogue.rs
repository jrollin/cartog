//! Client catalogue: known MCP clients, their config shapes, and path resolution.

use std::path::{Path, PathBuf};

use super::{ClientSpec, MergeStrategy, Scope};
use crate::cli::{ClientKind, IdeScope};

/// Static catalogue of every client cartog knows about: kind, scope, and JSON/TOML
/// shape. Adding a new client = one row in this table + matching path resolution
/// in [`HomeDirs::detect`] (for User-scope rows) or a `cwd.join(...)` in
/// [`project_path`] (for Project rows). Every client registers `serve --watch`
/// (see [`serve_args`]); `--no-watch` drops `--watch` for all of them.
pub(super) const CLIENT_CATALOGUE: &[CatalogueEntry] = &[
    CatalogueEntry {
        kind: ClientKind::ClaudeCode,
        scope: Scope::Project,
        strategy: MergeStrategy::McpServers,
    },
    CatalogueEntry {
        kind: ClientKind::ClaudeCode,
        scope: Scope::User,
        strategy: MergeStrategy::McpServers,
    },
    CatalogueEntry {
        kind: ClientKind::Cursor,
        scope: Scope::Project,
        strategy: MergeStrategy::McpServers,
    },
    CatalogueEntry {
        kind: ClientKind::Vscode,
        scope: Scope::Project,
        strategy: MergeStrategy::VsCodeServers,
    },
    CatalogueEntry {
        kind: ClientKind::Vscode,
        scope: Scope::User,
        strategy: MergeStrategy::VsCodeServers,
    },
    CatalogueEntry {
        kind: ClientKind::ClaudeDesktop,
        scope: Scope::User,
        strategy: MergeStrategy::McpServers,
    },
    CatalogueEntry {
        kind: ClientKind::Windsurf,
        scope: Scope::User,
        strategy: MergeStrategy::McpServers,
    },
    CatalogueEntry {
        kind: ClientKind::Opencode,
        scope: Scope::User,
        strategy: MergeStrategy::Mcp,
    },
    CatalogueEntry {
        kind: ClientKind::Zed,
        scope: Scope::User,
        strategy: MergeStrategy::ContextServers,
    },
    CatalogueEntry {
        kind: ClientKind::Codex,
        scope: Scope::User,
        strategy: MergeStrategy::CodexToml,
    },
    CatalogueEntry {
        kind: ClientKind::Gemini,
        scope: Scope::User,
        strategy: MergeStrategy::McpServers,
    },
    CatalogueEntry {
        kind: ClientKind::Antigravity,
        scope: Scope::User,
        strategy: MergeStrategy::McpServers,
    },
    CatalogueEntry {
        kind: ClientKind::Kiro,
        scope: Scope::Project,
        strategy: MergeStrategy::McpServers,
    },
    CatalogueEntry {
        kind: ClientKind::Kiro,
        scope: Scope::User,
        strategy: MergeStrategy::McpServers,
    },
    CatalogueEntry {
        kind: ClientKind::Hermes,
        scope: Scope::User,
        strategy: MergeStrategy::HermesYaml,
    },
];

pub(super) struct CatalogueEntry {
    pub(super) kind: ClientKind,
    pub(super) scope: Scope,
    pub(super) strategy: MergeStrategy,
}

/// Serve args every client is wired with: `["serve", "--watch"]`, dropping
/// `--watch` when `--no-watch` was given. Concurrent watchers are safe — the
/// single-writer election makes secondaries skip their own watcher.
pub(super) fn serve_args(no_watch: bool) -> Vec<String> {
    if no_watch {
        vec!["serve".into()]
    } else {
        vec!["serve".into(), "--watch".into()]
    }
}

/// Resolve the on-disk config path for a project-scoped client. Mirrors the
/// per-platform user paths assembled in [`HomeDirs::detect`].
pub(super) fn project_path(kind: ClientKind, cwd: &Path) -> Option<PathBuf> {
    match kind {
        ClientKind::ClaudeCode => Some(cwd.join(".mcp.json")),
        ClientKind::Cursor => Some(cwd.join(".cursor").join("mcp.json")),
        ClientKind::Vscode => Some(cwd.join(".vscode").join("mcp.json")),
        ClientKind::Kiro => Some(cwd.join(".kiro").join("settings").join("mcp.json")),
        // User-only clients have no project-scope analogue.
        _ => None,
    }
}

/// Look up the user-scope path for a client; mirror of `project_path` for the
/// User branch of the catalogue.
pub(super) fn user_path(kind: ClientKind, home: &HomeDirs) -> Option<PathBuf> {
    match kind {
        ClientKind::ClaudeCode => Some(home.claude_code.clone()),
        ClientKind::ClaudeDesktop => Some(home.claude_desktop.clone()),
        ClientKind::Codex => Some(home.codex.clone()),
        ClientKind::Gemini => Some(home.gemini.clone()),
        ClientKind::Opencode => Some(home.opencode.clone()),
        ClientKind::Windsurf => Some(home.windsurf.clone()),
        ClientKind::Zed => Some(home.zed.clone()),
        ClientKind::Antigravity => Some(home.antigravity.clone()),
        ClientKind::Hermes => Some(home.hermes.clone()),
        ClientKind::Kiro => Some(home.kiro.clone()),
        // Project-only clients have no user-scope analogue.
        ClientKind::Vscode => Some(home.vscode.clone()),
        // Cursor is project-scope only in cartog's catalogue.
        ClientKind::Cursor => None,
    }
}

/// Resolve the set of `ClientSpec`s to operate on, filtered by `client` and `scope`.
///
/// Override the resolution roots via `cwd` (project files) and `home_dirs`
/// (user-scoped files) for test sandboxing.
pub fn build_specs(
    client: Option<ClientKind>,
    scope: IdeScope,
    no_watch: bool,
    cwd: &Path,
    home_dirs: &HomeDirs,
) -> Vec<ClientSpec> {
    CLIENT_CATALOGUE
        .iter()
        .filter(|e| match scope {
            IdeScope::Project => e.scope == Scope::Project,
            IdeScope::User => e.scope == Scope::User,
            IdeScope::All => true,
        })
        .filter(|e| client.map_or(true, |c| c == e.kind))
        .filter_map(|e| {
            let path = match e.scope {
                Scope::Project => project_path(e.kind, cwd)?,
                Scope::User => user_path(e.kind, home_dirs)?,
            };
            Some(ClientSpec {
                kind: e.kind,
                scope: e.scope,
                path,
                strategy: e.strategy,
                args: serve_args(no_watch),
            })
        })
        .collect()
}

/// User-scope config file paths. Resolution is unconditional: each client gets a
/// canonical path per platform. Whether to actually write is decided at runtime
/// by checking whether the parent directory exists (proxy for "client installed").
#[derive(Debug, Clone)]
pub struct HomeDirs {
    pub claude_code: PathBuf,
    pub claude_desktop: PathBuf,
    pub codex: PathBuf,
    pub gemini: PathBuf,
    pub windsurf: PathBuf,
    pub opencode: PathBuf,
    pub zed: PathBuf,
    pub antigravity: PathBuf,
    pub hermes: PathBuf,
    pub kiro: PathBuf,
    pub vscode: PathBuf,
}

impl Default for HomeDirs {
    /// Fallback used when `directories::BaseDirs::new()` returns `None` (no
    /// resolvable home dir). All paths are anchored at `"."`, which guarantees
    /// the parent-dir check at write time returns "not installed" for every user
    /// client and we degrade gracefully to project-scope work.
    fn default() -> Self {
        let stub = PathBuf::from(".");
        HomeDirs {
            claude_code: stub.clone(),
            claude_desktop: stub.clone(),
            codex: stub.clone(),
            gemini: stub.clone(),
            windsurf: stub.clone(),
            opencode: stub.clone(),
            zed: stub.clone(),
            antigravity: stub.clone(),
            hermes: stub.clone(),
            kiro: stub.clone(),
            vscode: stub,
        }
    }
}

impl HomeDirs {
    /// Resolve config paths from the current environment.
    pub fn detect() -> Self {
        let Some(base) = directories::BaseDirs::new() else {
            return Self::default();
        };
        let home = base.home_dir().to_path_buf();

        // Zed and OpenCode store config under `~/.config/` on every platform, so
        // honour `XDG_CONFIG_HOME` first and fall back to a plain `~/.config`,
        // rather than `BaseDirs::config_dir()` (which is `~/Library/Application Support`
        // on macOS and would point at the wrong location).
        let xdg_config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));

        let claude_desktop = if cfg!(target_os = "macos") {
            home.join("Library/Application Support/Claude/claude_desktop_config.json")
        } else if cfg!(target_os = "windows") {
            base.config_dir().join("Claude/claude_desktop_config.json")
        } else {
            // Unofficial Linux builds (claude-desktop-debian and similar) store
            // config under XDG_CONFIG_HOME/Claude. Still gated by the parent-dir
            // existence check at write time, so users without it get a clean Skipped.
            xdg_config.join("Claude/claude_desktop_config.json")
        };

        // <config>/Code/User: Library/Application Support (macOS), %APPDATA%
        // (Windows), ~/.config (Linux) — VS Code uses config_dir, not xdg_config.
        let vscode = base.config_dir().join("Code/User/mcp.json");

        HomeDirs {
            claude_code: home.join(".claude/settings.json"),
            claude_desktop,
            codex: home.join(".codex/config.toml"),
            gemini: home.join(".gemini/settings.json"),
            windsurf: home.join(".codeium/windsurf/mcp_config.json"),
            opencode: xdg_config.join("opencode/opencode.json"),
            zed: xdg_config.join("zed/settings.json"),
            antigravity: home.join(".gemini/config/mcp_config.json"),
            hermes: home.join(".hermes/config.yaml"),
            kiro: home.join(".kiro/settings/mcp.json"),
            vscode,
        }
    }
}

/// CLI binary name for a client, if it ships one we can detect on `PATH`.
fn client_binary(kind: ClientKind) -> Option<&'static str> {
    match kind {
        ClientKind::ClaudeCode => Some("claude"),
        ClientKind::Codex => Some("codex"),
        ClientKind::Gemini => Some("gemini"),
        ClientKind::Opencode => Some("opencode"),
        ClientKind::Windsurf => Some("windsurf"),
        ClientKind::Zed => Some("zed"),
        ClientKind::Hermes => Some("hermes"),
        ClientKind::Kiro => Some("kiro"),
        // No reliable CLI: GUI apps (Claude Desktop, Antigravity) or editor-embedded (Cursor, VS Code).
        ClientKind::ClaudeDesktop
        | ClientKind::Cursor
        | ClientKind::Vscode
        | ClientKind::Antigravity => None,
    }
}

/// True if an executable `name` exists in any `paths` entry (PATH-style list).
/// Side-effect-free and injectable for tests. On Windows, also tries each
/// suffix in `%PATHEXT%` (falling back to a standard default) so `.bat`/`.com`
/// shims are found, not just `.exe`/`.cmd`.
pub(super) fn binary_in(paths: &std::ffi::OsStr, name: &str) -> bool {
    let mut candidates = vec![name.to_string()];
    if cfg!(windows) {
        let pathext =
            std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
        for ext in pathext.split(';').filter(|e| !e.is_empty()) {
            candidates.push(format!("{name}{}", ext.to_ascii_lowercase()));
        }
    }
    std::env::split_paths(paths).any(|dir| candidates.iter().any(|c| dir.join(c).is_file()))
}

/// Stronger "is this client installed?" check than parent-dir existence: a
/// known CLI on `path_env` (the PATH-style list, passed in for determinism +
/// testability) OR the config dir present. Project-scope clients are always
/// considered installable (the parent is the repo).
pub(super) fn client_installed(
    kind: ClientKind,
    scope: Scope,
    path: &Path,
    path_env: Option<&std::ffi::OsStr>,
) -> bool {
    if scope == Scope::Project {
        return true;
    }
    // Only trust a PATH match when we have a real home: `HomeDirs::default()`
    // (no resolvable home) anchors user paths at "." (relative), and wiring a
    // config there would litter the cwd. A genuine home path is absolute.
    if path.is_absolute() {
        if let (Some(bin), Some(p)) = (client_binary(kind), path_env) {
            if binary_in(p, bin) {
                return true;
            }
        }
    }
    // Fall back to the config-dir proxy (the only signal for GUI-only clients).
    path.parent().is_some_and(Path::exists)
}

/// Human-friendly client name for the picker. Mirrors `ClientKind`'s
/// `clap::ValueEnum` rendering but with capitalised display labels.
pub(super) fn client_display_name(kind: ClientKind) -> &'static str {
    match kind {
        ClientKind::ClaudeCode => "Claude Code",
        ClientKind::ClaudeDesktop => "Claude Desktop",
        ClientKind::Cursor => "Cursor",
        ClientKind::Vscode => "VS Code",
        ClientKind::Windsurf => "Windsurf",
        ClientKind::Opencode => "OpenCode",
        ClientKind::Zed => "Zed",
        ClientKind::Codex => "Codex CLI",
        ClientKind::Gemini => "Gemini CLI",
        ClientKind::Antigravity => "Antigravity",
        ClientKind::Hermes => "Hermes Agent",
        ClientKind::Kiro => "Kiro",
    }
}

/// Stable lowercase slug for a client (used in report output and paths).
pub(super) fn client_name(kind: ClientKind) -> &'static str {
    match kind {
        ClientKind::ClaudeCode => "claude-code",
        ClientKind::ClaudeDesktop => "claude-desktop",
        ClientKind::Codex => "codex",
        ClientKind::Cursor => "cursor",
        ClientKind::Gemini => "gemini",
        ClientKind::Opencode => "opencode",
        ClientKind::Vscode => "vscode",
        ClientKind::Windsurf => "windsurf",
        ClientKind::Zed => "zed",
        ClientKind::Antigravity => "antigravity",
        ClientKind::Hermes => "hermes",
        ClientKind::Kiro => "kiro",
    }
}
