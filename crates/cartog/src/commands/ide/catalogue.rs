//! Client catalogue: known MCP clients, their config shapes, and path resolution.

use std::path::{Path, PathBuf};

use super::{ClientSpec, Detection, MergeStrategy, Scope};
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
    CatalogueEntry {
        kind: ClientKind::Cursor,
        scope: Scope::User,
        strategy: MergeStrategy::McpServers,
    },
    CatalogueEntry {
        kind: ClientKind::Qwen,
        scope: Scope::Project,
        strategy: MergeStrategy::McpServers,
    },
    CatalogueEntry {
        kind: ClientKind::Qwen,
        scope: Scope::User,
        strategy: MergeStrategy::McpServers,
    },
    CatalogueEntry {
        kind: ClientKind::CopilotCli,
        scope: Scope::User,
        strategy: MergeStrategy::CopilotLocal,
    },
    CatalogueEntry {
        kind: ClientKind::Cline,
        scope: Scope::User,
        strategy: MergeStrategy::McpServersToggle,
    },
    CatalogueEntry {
        kind: ClientKind::ClineCli,
        scope: Scope::User,
        strategy: MergeStrategy::McpServersToggle,
    },
    CatalogueEntry {
        kind: ClientKind::Roo,
        scope: Scope::Project,
        strategy: MergeStrategy::McpServersToggle,
    },
    CatalogueEntry {
        kind: ClientKind::Roo,
        scope: Scope::User,
        strategy: MergeStrategy::McpServersToggle,
    },
    CatalogueEntry {
        kind: ClientKind::Kilo,
        scope: Scope::Project,
        strategy: MergeStrategy::Mcp,
    },
    CatalogueEntry {
        kind: ClientKind::Kilo,
        scope: Scope::User,
        strategy: MergeStrategy::Mcp,
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
        ClientKind::Qwen => Some(cwd.join(".qwen").join("settings.json")),
        ClientKind::Roo => Some(cwd.join(".roo").join("mcp.json")),
        ClientKind::Kilo => Some(kilo_config_file(&cwd.join(".kilo"))),
        // User-only clients have no project-scope analogue.
        _ => None,
    }
}

/// Kilo loads `kilo.jsonc` or `kilo.json` from `dir`. Target an existing `.jsonc`
/// so cartog never creates a second config file beside the user's own.
pub(super) fn kilo_config_file(dir: &Path) -> PathBuf {
    let jsonc = dir.join("kilo.jsonc");
    if jsonc.exists() {
        jsonc
    } else {
        dir.join("kilo.json")
    }
}

/// Kilo's user dir also accepts `config.json`: target it when it is the only one
/// present, so cartog does not add a second config beside it.
pub(super) fn kilo_user_config_file(dir: &Path) -> PathBuf {
    let preferred = kilo_config_file(dir);
    let legacy = dir.join("config.json");
    if !preferred.exists() && legacy.exists() {
        legacy
    } else {
        preferred
    }
}

/// Copilot CLI's config dir: `COPILOT_HOME` replaces `~/.copilot` as a whole.
pub(super) fn copilot_dir(copilot_home: Option<std::ffi::OsString>, home: &Path) -> PathBuf {
    env_path_or(copilot_home, home.join(".copilot"))
}

/// A documented location override from the environment, or `default` when unset or empty.
pub(super) fn env_path_or(value: Option<std::ffi::OsString>, default: PathBuf) -> PathBuf {
    value
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or(default)
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
        ClientKind::Vscode => Some(home.vscode.clone()),
        ClientKind::Cursor => Some(home.cursor.clone()),
        ClientKind::Qwen => Some(home.qwen.clone()),
        ClientKind::CopilotCli => Some(home.copilot_cli.clone()),
        ClientKind::Cline => Some(home.cline.clone()),
        ClientKind::ClineCli => Some(home.cline_cli.clone()),
        ClientKind::Roo => Some(home.roo.clone()),
        ClientKind::Kilo => Some(home.kilo.clone()),
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
        .filter(|e| client.is_none_or(|c| c == e.kind))
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
                detection: detection(e.kind, e.scope, cwd, home_dirs),
                requires: required_dir(e.kind, e.scope, home_dirs),
            })
        })
        .collect()
}

/// User-scope config file paths. Resolution is unconditional: each client gets a
/// canonical path per platform. Whether auto mode writes is decided at runtime
/// by the row's `Detection` rule.
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
    pub cursor: PathBuf,
    pub qwen: PathBuf,
    pub copilot_cli: PathBuf,
    /// Copilot CLI's own `config.json`, written on first run.
    pub copilot_marker: PathBuf,
    pub cline: PathBuf,
    pub cline_cli: PathBuf,
    pub roo: PathBuf,
    pub kilo: PathBuf,
    /// Cursor's app data dir: only the Cursor app creates it.
    pub cursor_app: PathBuf,
    /// Kilo's VS Code extension storage: only the extension creates it.
    pub kilo_extension: PathBuf,
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
            vscode: stub.clone(),
            cursor: stub.clone(),
            qwen: stub.clone(),
            copilot_cli: stub.clone(),
            copilot_marker: stub.clone(),
            cline: stub.clone(),
            cline_cli: stub.clone(),
            roo: stub.clone(),
            kilo: stub.clone(),
            cursor_app: stub.clone(),
            kilo_extension: stub,
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
        // Extensions (Cline, Roo) keep their MCP settings in its globalStorage.
        let vscode_user = base.config_dir().join("Code/User");
        let copilot = copilot_dir(std::env::var_os("COPILOT_HOME"), &home);
        let vscode_ext = vscode_user.join("globalStorage");

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
            vscode: vscode_user.join("mcp.json"),
            cursor: home.join(".cursor/mcp.json"),
            qwen: home.join(".qwen/settings.json"),
            copilot_cli: copilot.join("mcp-config.json"),
            copilot_marker: copilot.join("config.json"),
            cline: vscode_ext.join("saoudrizwan.claude-dev/settings/cline_mcp_settings.json"),
            cline_cli: env_path_or(
                std::env::var_os("CLINE_MCP_SETTINGS_PATH"),
                home.join(".cline/data/settings/cline_mcp_settings.json"),
            ),
            roo: vscode_ext.join("rooveterinaryinc.roo-cline/settings/mcp_settings.json"),
            kilo: kilo_user_config_file(&xdg_config.join("kilo")),
            cursor_app: base.config_dir().join("Cursor"),
            kilo_extension: vscode_ext.join("kilocode.kilo-code"),
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
        ClientKind::Qwen => Some("qwen"),
        ClientKind::ClineCli => Some("cline"),
        // No reliable CLI: GUI apps (Claude Desktop, Antigravity) or editor-embedded
        // (Cursor, VS Code, and the Cline / Roo extensions).
        ClientKind::ClaudeDesktop
        | ClientKind::Cursor
        | ClientKind::Vscode
        | ClientKind::Antigravity
        | ClientKind::Cline
        | ClientKind::Roo => None,
        ClientKind::Kilo => Some("kilo"),
        // AWS Copilot CLI also installs `copilot`: GitHub Copilot CLI is detected
        // by its own config.json instead.
        ClientKind::CopilotCli => None,
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

/// The Cline and Roo extension files live inside VS Code's user dir: creating it for
/// them would make VS Code itself look installed to a later auto run.
pub(super) fn required_dir(kind: ClientKind, scope: Scope, homes: &HomeDirs) -> Option<PathBuf> {
    match (kind, scope) {
        (ClientKind::Cline | ClientKind::Roo, Scope::User) => {
            homes.vscode.parent().map(Path::to_path_buf)
        }
        _ => None,
    }
}

/// Auto-mode install rule for one catalogue row.
pub(super) fn detection(kind: ClientKind, scope: Scope, cwd: &Path, homes: &HomeDirs) -> Detection {
    match scope {
        Scope::Project => project_detection(kind, cwd, homes),
        Scope::User => user_detection(kind, homes),
    }
}

/// Newer clients' project files are only wired where the client is in use (its
/// folder is in the repo, or it is detected for the user), so an auto run does
/// not seed `.qwen/`, `.roo/` and `.kilo/` into every repository.
fn project_detection(kind: ClientKind, cwd: &Path, homes: &HomeDirs) -> Detection {
    let parent = |p: &Path| p.parent().map(Path::to_path_buf).unwrap_or_default();
    match kind {
        ClientKind::Qwen => Detection::Evidence(vec![cwd.join(".qwen"), homes.qwen.clone()]),
        ClientKind::Roo => Detection::Evidence(vec![cwd.join(".roo"), parent(&homes.roo)]),
        ClientKind::Kilo => Detection::Evidence(vec![
            cwd.join(".kilo"),
            homes.kilo.clone(),
            homes.kilo_extension.clone(),
        ]),
        ClientKind::ClaudeCode | ClientKind::Cursor | ClientKind::Kiro | ClientKind::Vscode => {
            Detection::Always
        }
        // No project row.
        ClientKind::Antigravity
        | ClientKind::ClaudeDesktop
        | ClientKind::Cline
        | ClientKind::ClineCli
        | ClientKind::Codex
        | ClientKind::CopilotCli
        | ClientKind::Gemini
        | ClientKind::Hermes
        | ClientKind::Opencode
        | ClientKind::Windsurf
        | ClientKind::Zed => Detection::Always,
    }
}

/// Skill installers (`npx skills add`) create `~/.qwen/skills`, `~/.cursor/skills`,
/// …, so for those clients the parent-dir proxy would report an install that is not there.
fn user_detection(kind: ClientKind, homes: &HomeDirs) -> Detection {
    match kind {
        ClientKind::Cursor => Detection::Evidence(vec![homes.cursor_app.clone()]),
        ClientKind::Kilo => Detection::Evidence(vec![homes.kilo_extension.clone()]),
        ClientKind::CopilotCli => Detection::Evidence(vec![homes.copilot_marker.clone()]),
        ClientKind::Qwen => Detection::Evidence(Vec::new()),
        ClientKind::Antigravity
        | ClientKind::ClaudeCode
        | ClientKind::ClaudeDesktop
        | ClientKind::Cline
        | ClientKind::ClineCli
        | ClientKind::Codex
        | ClientKind::Gemini
        | ClientKind::Hermes
        | ClientKind::Kiro
        | ClientKind::Opencode
        | ClientKind::Roo
        | ClientKind::Vscode
        | ClientKind::Windsurf
        | ClientKind::Zed => Detection::ParentDir,
    }
}

/// Stronger "is this client installed?" check than parent-dir existence: a
/// known CLI on `path_env` (the PATH-style list, passed in for determinism +
/// testability) OR the row's `detection` evidence.
pub(super) fn client_installed(
    kind: ClientKind,
    path: &Path,
    detection: &Detection,
    path_env: Option<&std::ffi::OsStr>,
) -> bool {
    // Only trust a PATH match or evidence paths when we have a real home:
    // `HomeDirs::default()` (no resolvable home) anchors user paths at "."
    // (relative), and wiring a config there would litter the cwd.
    let has_home = path.is_absolute();
    if has_home {
        if let (Some(bin), Some(p)) = (client_binary(kind), path_env) {
            if binary_in(p, bin) {
                return true;
            }
        }
    }
    match detection {
        Detection::Always => true,
        // The config-dir proxy (the only signal for GUI-only clients).
        Detection::ParentDir => path.parent().is_some_and(Path::exists),
        // Markers can mix the repo with user paths, which are "." stubs when there
        // is no home: only absolute ones count.
        Detection::Evidence(markers) => {
            has_home && (path.exists() || markers.iter().any(|m| m.is_absolute() && m.exists()))
        }
    }
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
        ClientKind::Qwen => "Qwen Code",
        ClientKind::CopilotCli => "Copilot CLI",
        ClientKind::Cline => "Cline",
        ClientKind::ClineCli => "Cline CLI",
        ClientKind::Roo => "Roo Code",
        ClientKind::Kilo => "Kilo Code",
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
        ClientKind::Qwen => "qwen",
        ClientKind::CopilotCli => "copilot-cli",
        ClientKind::Cline => "cline",
        ClientKind::ClineCli => "cline-cli",
        ClientKind::Roo => "roo",
        ClientKind::Kilo => "kilo",
    }
}
