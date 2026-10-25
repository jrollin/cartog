use super::args;
use crate::cli::{ClientKind, IdeScope};
use crate::commands::ide::catalogue::*;
use crate::commands::ide::merge::merge_entry;
use crate::commands::ide::{Detection, MergeStrategy, Scope};
use serde_json::Value;
use std::path::{Path, PathBuf};
#[test]
fn build_specs_default_covers_all_clients() {
    let tmp = std::env::temp_dir();
    let homes = HomeDirs::detect();
    let specs = build_specs(None, IdeScope::All, false, &tmp, &homes);
    // Project: claude-code, cursor, vscode, kiro, qwen, roo, kilo (7)
    // User: claude-code, claude-desktop, codex, gemini, opencode, windsurf, zed,
    //       antigravity, kiro, hermes, vscode, cursor, qwen, copilot-cli,
    //       cline, cline-cli, roo, kilo (18)
    assert_eq!(specs.len(), 25);
}

#[test]
fn build_specs_project_scope_drops_user_clients() {
    let tmp = std::env::temp_dir();
    let homes = HomeDirs::default();
    let specs = build_specs(None, IdeScope::Project, false, &tmp, &homes);
    // claude-code, cursor, vscode, kiro, qwen, roo, kilo
    assert_eq!(specs.len(), 7);
    assert!(specs.iter().all(|s| s.scope == Scope::Project));
}

#[test]
fn build_specs_claude_code_filter_returns_both_scopes() {
    let tmp = std::env::temp_dir();
    let homes = HomeDirs::default();
    let specs = build_specs(
        Some(ClientKind::ClaudeCode),
        IdeScope::All,
        false,
        &tmp,
        &homes,
    );
    assert_eq!(specs.len(), 2);
    let scopes: Vec<_> = specs.iter().map(|s| s.scope).collect();
    assert!(scopes.contains(&Scope::Project));
    assert!(scopes.contains(&Scope::User));
}

#[test]
fn build_specs_vscode_filter_returns_project_and_user() {
    let tmp = std::env::temp_dir();
    let homes = HomeDirs {
        vscode: tmp.join("Code/User/mcp.json"),
        ..HomeDirs::default()
    };
    let specs = build_specs(
        Some(ClientKind::Vscode),
        IdeScope::All,
        false,
        tmp.as_path(),
        &homes,
    );
    let scopes: Vec<_> = specs.iter().map(|s| s.scope).collect();
    assert!(
        scopes.contains(&Scope::Project),
        "vscode missing project scope"
    );
    assert!(scopes.contains(&Scope::User), "vscode missing user scope");
    let user = specs
        .iter()
        .find(|s| s.scope == Scope::User)
        .expect("vscode user spec");
    assert!(user.path.ends_with("Code/User/mcp.json"));
    assert_eq!(user.strategy, MergeStrategy::VsCodeServers);
}

#[test]
fn detect_vscode_user_path_under_config_dir() {
    // VS Code's user mcp.json lives at <config>/Code/User/mcp.json on every OS.
    let homes = HomeDirs::detect();
    assert!(
        homes.vscode.ends_with("Code/User/mcp.json"),
        "unexpected vscode user path: {}",
        homes.vscode.display()
    );
}

#[test]
fn build_specs_client_filter_picks_one() {
    let tmp = std::env::temp_dir();
    let homes = HomeDirs::default();
    let specs = build_specs(
        Some(ClientKind::Windsurf),
        IdeScope::All,
        false,
        &tmp,
        &homes,
    );
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].kind, ClientKind::Windsurf);
}

#[test]
fn build_specs_kiro_filter_returns_both_scopes() {
    let tmp = std::env::temp_dir();
    let homes = HomeDirs::default();
    let specs = build_specs(Some(ClientKind::Kiro), IdeScope::All, false, &tmp, &homes);
    assert_eq!(specs.len(), 2);
    let scopes: Vec<_> = specs.iter().map(|s| s.scope).collect();
    assert!(scopes.contains(&Scope::Project));
    assert!(scopes.contains(&Scope::User));
}

#[test]
fn kiro_project_path_is_kiro_settings_mcp_json() {
    let cwd = Path::new("/tmp/proj");
    assert_eq!(
        project_path(ClientKind::Kiro, cwd),
        Some(cwd.join(".kiro").join("settings").join("mcp.json"))
    );
}

#[test]
fn antigravity_and_hermes_are_user_only() {
    let cwd = Path::new("/tmp/proj");
    assert_eq!(project_path(ClientKind::Antigravity, cwd), None);
    assert_eq!(project_path(ClientKind::Hermes, cwd), None);
}

#[test]
fn antigravity_and_kiro_use_mcp_servers_shape() {
    // Both reuse the existing McpServers JSON strategy.
    let o = merge_entry(None, MergeStrategy::McpServers, &args()).unwrap();
    let v: Value = serde_json::from_str(&o.new_json).unwrap();
    assert_eq!(v["mcpServers"]["cartog"]["command"], "cartog");
}

#[test]
fn cursor_has_project_and_user_rows() {
    let cwd = Path::new("/tmp/proj");
    let homes = HomeDirs {
        cursor: PathBuf::from("/home/u/.cursor/mcp.json"),
        ..HomeDirs::default()
    };
    let specs = build_specs(Some(ClientKind::Cursor), IdeScope::All, false, cwd, &homes);
    let paths: Vec<_> = specs.iter().map(|s| (s.scope, s.path.clone())).collect();
    assert_eq!(
        paths,
        vec![
            (Scope::Project, cwd.join(".cursor").join("mcp.json")),
            (Scope::User, PathBuf::from("/home/u/.cursor/mcp.json")),
        ]
    );
    assert!(specs
        .iter()
        .all(|s| s.strategy == MergeStrategy::McpServers));
}

#[test]
fn cursor_project_scope_still_writes_only_the_project_file() {
    let cwd = Path::new("/tmp/proj");
    let specs = build_specs(
        Some(ClientKind::Cursor),
        IdeScope::Project,
        false,
        cwd,
        &HomeDirs::default(),
    );
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].path, cwd.join(".cursor").join("mcp.json"));
}

#[test]
fn detect_cursor_user_path_is_home_dot_cursor() {
    let homes = HomeDirs::detect();
    assert!(
        homes.cursor.ends_with(".cursor/mcp.json"),
        "unexpected cursor user path: {}",
        homes.cursor.display()
    );
}

#[test]
fn qwen_has_project_and_user_rows_with_mcp_servers_shape() {
    let cwd = Path::new("/tmp/proj");
    let homes = HomeDirs {
        qwen: PathBuf::from("/home/u/.qwen/settings.json"),
        ..HomeDirs::default()
    };
    let specs = build_specs(Some(ClientKind::Qwen), IdeScope::All, false, cwd, &homes);
    let paths: Vec<_> = specs.iter().map(|s| (s.scope, s.path.clone())).collect();
    assert_eq!(
        paths,
        vec![
            (Scope::Project, cwd.join(".qwen").join("settings.json")),
            (Scope::User, PathBuf::from("/home/u/.qwen/settings.json")),
        ]
    );
    assert!(specs
        .iter()
        .all(|s| s.strategy == MergeStrategy::McpServers));
}

#[test]
fn detect_qwen_user_path_is_home_dot_qwen() {
    let homes = HomeDirs::detect();
    assert!(
        homes.qwen.ends_with(".qwen/settings.json"),
        "unexpected qwen user path: {}",
        homes.qwen.display()
    );
}

#[test]
fn new_rows_honour_no_watch() {
    let cwd = Path::new("/tmp/proj");
    let homes = HomeDirs::default();
    for kind in [ClientKind::Cursor, ClientKind::Qwen] {
        let watched = build_specs(Some(kind), IdeScope::All, false, cwd, &homes);
        assert!(watched.iter().all(|s| s.args == ["serve", "--watch"]));
        let unwatched = build_specs(Some(kind), IdeScope::All, true, cwd, &homes);
        assert!(unwatched.iter().all(|s| s.args == ["serve"]));
    }
}

#[test]
fn qwen_binary_on_path_counts_as_installed() {
    let bin_dir = tempfile::TempDir::new().unwrap();
    let exe = if cfg!(windows) { "qwen.exe" } else { "qwen" };
    std::fs::write(bin_dir.path().join(exe), "").unwrap();
    let missing = bin_dir.path().join("no-such-dir/settings.json");
    assert!(client_installed(
        ClientKind::Qwen,
        &missing,
        &detection(
            ClientKind::Qwen,
            Scope::User,
            Path::new("/tmp/proj"),
            &HomeDirs::default()
        ),
        Some(bin_dir.path().as_os_str()),
    ));
}

#[test]
fn copilot_cli_is_user_only_with_copilot_local_shape() {
    let cwd = Path::new("/tmp/proj");
    let homes = HomeDirs {
        copilot_cli: PathBuf::from("/home/u/.copilot/mcp-config.json"),
        ..HomeDirs::default()
    };
    let specs = build_specs(
        Some(ClientKind::CopilotCli),
        IdeScope::All,
        false,
        cwd,
        &homes,
    );
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].scope, Scope::User);
    assert_eq!(
        specs[0].path,
        PathBuf::from("/home/u/.copilot/mcp-config.json")
    );
    assert_eq!(specs[0].strategy, MergeStrategy::CopilotLocal);
}

#[test]
fn copilot_cli_has_no_project_row() {
    let cwd = Path::new("/tmp/proj");
    let specs = build_specs(
        Some(ClientKind::CopilotCli),
        IdeScope::Project,
        false,
        cwd,
        &HomeDirs::default(),
    );
    assert!(specs.is_empty());
}

#[test]
fn detect_cline_and_roo_extension_paths_share_the_vscode_user_dir() {
    let homes = HomeDirs::detect();
    let vscode_user = homes.vscode.parent().expect("vscode path has a parent");
    assert_eq!(
        homes.cline,
        vscode_user.join("globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json")
    );
    assert_eq!(
        homes.roo,
        vscode_user.join("globalStorage/rooveterinaryinc.roo-cline/settings/mcp_settings.json")
    );
}

#[test]
fn roo_has_project_and_user_rows() {
    let cwd = Path::new("/tmp/proj");
    let homes = HomeDirs {
        roo: PathBuf::from("/vscode/globalStorage/roo/settings/mcp_settings.json"),
        ..HomeDirs::default()
    };
    let specs = build_specs(Some(ClientKind::Roo), IdeScope::All, false, cwd, &homes);
    let paths: Vec<_> = specs.iter().map(|s| (s.scope, s.path.clone())).collect();
    assert_eq!(
        paths,
        vec![
            (Scope::Project, cwd.join(".roo").join("mcp.json")),
            (
                Scope::User,
                PathBuf::from("/vscode/globalStorage/roo/settings/mcp_settings.json")
            ),
        ]
    );
}

#[test]
fn cline_kinds_are_user_only() {
    let cwd = Path::new("/tmp/proj");
    assert_eq!(project_path(ClientKind::Cline, cwd), None);
    assert_eq!(project_path(ClientKind::ClineCli, cwd), None);
}

#[test]
fn cline_cline_cli_and_roo_rows_keep_client_owned_fields() {
    let cwd = Path::new("/tmp/proj");
    let homes = HomeDirs::default();
    for kind in [ClientKind::Cline, ClientKind::ClineCli, ClientKind::Roo] {
        let specs = build_specs(Some(kind), IdeScope::All, false, cwd, &homes);
        assert!(!specs.is_empty(), "{kind:?} has no rows");
        assert!(
            specs
                .iter()
                .all(|s| s.strategy == MergeStrategy::McpServersToggle),
            "{kind:?} must use McpServersToggle"
        );
    }
}

#[test]
fn cline_binary_marks_only_the_cli_installed() {
    let bin_dir = tempfile::TempDir::new().unwrap();
    let exe = if cfg!(windows) { "cline.exe" } else { "cline" };
    std::fs::write(bin_dir.path().join(exe), "").unwrap();
    let missing = bin_dir.path().join("no-such-dir/cline_mcp_settings.json");
    let path_env = Some(bin_dir.path().as_os_str());
    let homes = HomeDirs::default();
    assert!(client_installed(
        ClientKind::ClineCli,
        &missing,
        &detection(
            ClientKind::ClineCli,
            Scope::User,
            Path::new("/tmp/proj"),
            &homes
        ),
        path_env
    ));
    assert!(!client_installed(
        ClientKind::Cline,
        &missing,
        &detection(
            ClientKind::Cline,
            Scope::User,
            Path::new("/tmp/proj"),
            &homes
        ),
        path_env
    ));
}

#[test]
fn kilo_config_file_defaults_to_json() {
    let dir = tempfile::TempDir::new().unwrap();
    assert_eq!(kilo_config_file(dir.path()), dir.path().join("kilo.json"));
}

#[test]
fn kilo_config_file_targets_an_existing_jsonc() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("kilo.jsonc"), "{}").unwrap();
    assert_eq!(kilo_config_file(dir.path()), dir.path().join("kilo.jsonc"));
}

#[test]
fn kilo_config_file_prefers_jsonc_when_both_exist() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("kilo.jsonc"), "{}").unwrap();
    std::fs::write(dir.path().join("kilo.json"), "{}").unwrap();
    assert_eq!(kilo_config_file(dir.path()), dir.path().join("kilo.jsonc"));
}

#[test]
fn kilo_project_path_uses_the_dot_kilo_dir() {
    let cwd = tempfile::TempDir::new().unwrap();
    let kilo_dir = cwd.path().join(".kilo");
    assert_eq!(
        project_path(ClientKind::Kilo, cwd.path()),
        Some(kilo_dir.join("kilo.json"))
    );
    std::fs::create_dir_all(&kilo_dir).unwrap();
    std::fs::write(kilo_dir.join("kilo.jsonc"), "{}").unwrap();
    assert_eq!(
        project_path(ClientKind::Kilo, cwd.path()),
        Some(kilo_dir.join("kilo.jsonc"))
    );
}

#[test]
fn detect_kilo_user_path_is_under_config_kilo() {
    let homes = HomeDirs::detect();
    let parent = homes.kilo.parent().expect("kilo path has a parent");
    assert!(
        parent.ends_with("kilo"),
        "unexpected: {}",
        homes.kilo.display()
    );
    let name = homes.kilo.file_name().and_then(|n| n.to_str());
    assert!(
        matches!(name, Some("kilo.json") | Some("kilo.jsonc")),
        "unexpected file: {}",
        homes.kilo.display()
    );
}

#[test]
fn kilo_has_project_and_user_rows_with_opencode_shape() {
    let cwd = Path::new("/tmp/proj");
    let homes = HomeDirs::default();
    let specs = build_specs(Some(ClientKind::Kilo), IdeScope::All, false, cwd, &homes);
    let scopes: Vec<_> = specs.iter().map(|s| s.scope).collect();
    assert_eq!(scopes, vec![Scope::Project, Scope::User]);
    assert!(specs.iter().all(|s| s.strategy == MergeStrategy::Mcp));
}

fn no_binaries() -> std::ffi::OsString {
    std::env::join_paths::<_, &Path>([]).unwrap()
}

fn installed(kind: ClientKind, cfg: &Path, homes: &HomeDirs) -> bool {
    client_installed(
        kind,
        cfg,
        &detection(kind, Scope::User, Path::new("/tmp/proj"), homes),
        Some(&no_binaries()),
    )
}

fn project_installed(
    kind: ClientKind,
    cwd: &Path,
    homes: &HomeDirs,
    path_env: &std::ffi::OsStr,
) -> bool {
    let cfg = project_path(kind, cwd).expect("project row");
    client_installed(
        kind,
        &cfg,
        &detection(kind, Scope::Project, cwd, homes),
        Some(path_env),
    )
}

#[test]
fn qwen_home_with_only_skills_is_not_installed() {
    let home = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(home.path().join(".qwen/skills")).unwrap();
    let cfg = home.path().join(".qwen/settings.json");
    assert!(!installed(ClientKind::Qwen, &cfg, &HomeDirs::default()));
}

#[test]
fn qwen_settings_file_counts_as_installed() {
    let home = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(home.path().join(".qwen")).unwrap();
    let cfg = home.path().join(".qwen/settings.json");
    std::fs::write(&cfg, "{}").unwrap();
    assert!(installed(ClientKind::Qwen, &cfg, &HomeDirs::default()));
}

#[test]
fn cursor_home_with_only_skills_is_not_installed() {
    let home = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(home.path().join(".cursor/skills")).unwrap();
    let homes = HomeDirs {
        cursor_app: home.path().join("AppSupport/Cursor"),
        ..HomeDirs::default()
    };
    let cfg = home.path().join(".cursor/mcp.json");
    assert!(!installed(ClientKind::Cursor, &cfg, &homes));
}

#[test]
fn cursor_app_data_dir_counts_as_installed() {
    let home = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(home.path().join("AppSupport/Cursor")).unwrap();
    let homes = HomeDirs {
        cursor_app: home.path().join("AppSupport/Cursor"),
        ..HomeDirs::default()
    };
    let cfg = home.path().join(".cursor/mcp.json");
    assert!(installed(ClientKind::Cursor, &cfg, &homes));
}

#[test]
fn kilo_extension_storage_counts_as_installed() {
    let home = tempfile::TempDir::new().unwrap();
    let ext = home
        .path()
        .join("Code/User/globalStorage/kilocode.kilo-code");
    std::fs::create_dir_all(&ext).unwrap();
    let homes = HomeDirs {
        kilo_extension: ext,
        ..HomeDirs::default()
    };
    let cfg = home.path().join(".config/kilo/kilo.json");
    assert!(installed(ClientKind::Kilo, &cfg, &homes));
}

#[test]
fn cline_extension_settings_dir_counts_as_installed_without_a_binary() {
    let home = tempfile::TempDir::new().unwrap();
    let settings = home
        .path()
        .join("Code/User/globalStorage/saoudrizwan.claude-dev/settings");
    std::fs::create_dir_all(&settings).unwrap();
    let cfg = settings.join("cline_mcp_settings.json");
    assert!(installed(ClientKind::Cline, &cfg, &HomeDirs::default()));
}

#[test]
fn evidence_rule_ignores_the_relative_no_home_fallback() {
    // HomeDirs::default() anchors every path at "." which exists; it must not
    // read as an installed client.
    let homes = HomeDirs::default();
    for kind in [ClientKind::Qwen, ClientKind::Cursor, ClientKind::Kilo] {
        let cfg = user_path(kind, &homes).unwrap();
        assert!(!installed(kind, &cfg, &homes), "{kind:?}");
    }
}

#[test]
fn only_the_new_at_risk_rows_use_evidence_detection() {
    let homes = HomeDirs::default();
    for kind in [
        ClientKind::Gemini,
        ClientKind::Kiro,
        ClientKind::Cline,
        ClientKind::Roo,
    ] {
        assert_eq!(
            detection(kind, Scope::User, Path::new("/tmp/proj"), &homes),
            Detection::ParentDir,
            "{kind:?}"
        );
    }
    for kind in [
        ClientKind::Cursor,
        ClientKind::CopilotCli,
        ClientKind::Qwen,
        ClientKind::Kilo,
    ] {
        assert!(
            matches!(
                detection(kind, Scope::User, Path::new("/tmp/proj"), &homes),
                Detection::Evidence(_)
            ),
            "{kind:?}"
        );
    }
}

#[test]
fn detect_marker_paths() {
    let homes = HomeDirs::detect();
    assert!(
        homes.cursor_app.ends_with("Cursor"),
        "{}",
        homes.cursor_app.display()
    );
    assert!(
        homes
            .kilo_extension
            .ends_with("Code/User/globalStorage/kilocode.kilo-code"),
        "{}",
        homes.kilo_extension.display()
    );
}

#[test]
fn new_project_rows_need_a_folder_or_a_user_scope_detection() {
    let cwd = tempfile::TempDir::new().unwrap();
    let homes = HomeDirs::default();
    for kind in [ClientKind::Qwen, ClientKind::Roo, ClientKind::Kilo] {
        assert!(
            !project_installed(kind, cwd.path(), &homes, &no_binaries()),
            "{kind:?} project row written in a bare repo"
        );
    }
}

#[test]
fn roo_project_folder_installs_the_roo_project_row() {
    let cwd = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(cwd.path().join(".roo/rules")).unwrap();
    assert!(project_installed(
        ClientKind::Roo,
        cwd.path(),
        &HomeDirs::default(),
        &no_binaries()
    ));
}

#[test]
fn roo_extension_installs_the_roo_project_row() {
    let cwd = tempfile::TempDir::new().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    let settings = home
        .path()
        .join("globalStorage/rooveterinaryinc.roo-cline/settings");
    std::fs::create_dir_all(&settings).unwrap();
    let homes = HomeDirs {
        roo: settings.join("mcp_settings.json"),
        ..HomeDirs::default()
    };
    assert!(project_installed(
        ClientKind::Roo,
        cwd.path(),
        &homes,
        &no_binaries()
    ));
}

#[test]
fn qwen_on_path_installs_the_qwen_project_row() {
    let cwd = tempfile::TempDir::new().unwrap();
    let bin_dir = tempfile::TempDir::new().unwrap();
    let exe = if cfg!(windows) { "qwen.exe" } else { "qwen" };
    std::fs::write(bin_dir.path().join(exe), "").unwrap();
    assert!(project_installed(
        ClientKind::Qwen,
        cwd.path(),
        &HomeDirs::default(),
        bin_dir.path().as_os_str()
    ));
}

#[test]
fn kilo_extension_installs_the_kilo_project_row() {
    let cwd = tempfile::TempDir::new().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    let ext = home.path().join("globalStorage/kilocode.kilo-code");
    std::fs::create_dir_all(&ext).unwrap();
    let homes = HomeDirs {
        kilo_extension: ext,
        ..HomeDirs::default()
    };
    assert!(project_installed(
        ClientKind::Kilo,
        cwd.path(),
        &homes,
        &no_binaries()
    ));
}

#[test]
fn existing_project_rows_are_always_installed() {
    let cwd = tempfile::TempDir::new().unwrap();
    let homes = HomeDirs::default();
    for kind in [
        ClientKind::ClaudeCode,
        ClientKind::Cursor,
        ClientKind::Vscode,
        ClientKind::Kiro,
    ] {
        assert_eq!(
            detection(kind, Scope::Project, cwd.path(), &homes),
            Detection::Always,
            "{kind:?}"
        );
        assert!(
            project_installed(kind, cwd.path(), &homes, &no_binaries()),
            "{kind:?}"
        );
    }
}

#[test]
fn copilot_binary_alone_is_not_a_copilot_cli_install() {
    // AWS Copilot CLI also installs a `copilot` binary.
    let bin_dir = tempfile::TempDir::new().unwrap();
    let exe = if cfg!(windows) {
        "copilot.exe"
    } else {
        "copilot"
    };
    std::fs::write(bin_dir.path().join(exe), "").unwrap();
    let home = tempfile::TempDir::new().unwrap();
    let homes = HomeDirs {
        copilot_marker: home.path().join(".copilot/config.json"),
        ..HomeDirs::default()
    };
    let cfg = home.path().join(".copilot/mcp-config.json");
    assert!(!client_installed(
        ClientKind::CopilotCli,
        &cfg,
        &detection(
            ClientKind::CopilotCli,
            Scope::User,
            Path::new("/tmp/proj"),
            &homes
        ),
        Some(bin_dir.path().as_os_str()),
    ));
}

#[test]
fn copilot_config_json_marks_copilot_cli_installed() {
    let home = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(home.path().join(".copilot")).unwrap();
    std::fs::write(home.path().join(".copilot/config.json"), "{}").unwrap();
    let homes = HomeDirs {
        copilot_marker: home.path().join(".copilot/config.json"),
        ..HomeDirs::default()
    };
    let cfg = home.path().join(".copilot/mcp-config.json");
    assert!(installed(ClientKind::CopilotCli, &cfg, &homes));
}

#[test]
fn copilot_home_relocates_the_whole_copilot_dir() {
    let home = Path::new("/home/u");
    assert_eq!(copilot_dir(None, home), home.join(".copilot"));
    assert_eq!(copilot_dir(Some("".into()), home), home.join(".copilot"));
    assert_eq!(
        copilot_dir(Some("/srv/copilot".into()), home),
        PathBuf::from("/srv/copilot")
    );
}

#[test]
fn cline_mcp_settings_path_replaces_the_cline_cli_file() {
    let default = PathBuf::from("/home/u/.cline/data/settings/cline_mcp_settings.json");
    assert_eq!(env_path_or(None, default.clone()), default);
    assert_eq!(env_path_or(Some("".into()), default.clone()), default);
    assert_eq!(
        env_path_or(Some("/etc/cline/mcp.json".into()), default),
        PathBuf::from("/etc/cline/mcp.json")
    );
}

#[test]
fn kilo_binary_on_path_counts_as_installed() {
    let bin_dir = tempfile::TempDir::new().unwrap();
    let exe = if cfg!(windows) { "kilo.exe" } else { "kilo" };
    std::fs::write(bin_dir.path().join(exe), "").unwrap();
    let missing = bin_dir.path().join("no-such-dir/kilo.json");
    assert!(client_installed(
        ClientKind::Kilo,
        &missing,
        &detection(
            ClientKind::Kilo,
            Scope::User,
            Path::new("/tmp/proj"),
            &HomeDirs::default()
        ),
        Some(bin_dir.path().as_os_str()),
    ));
}

#[test]
fn kilo_user_config_targets_an_existing_config_json() {
    let dir = tempfile::TempDir::new().unwrap();
    assert_eq!(
        kilo_user_config_file(dir.path()),
        dir.path().join("kilo.json")
    );
    std::fs::write(dir.path().join("config.json"), "{}").unwrap();
    assert_eq!(
        kilo_user_config_file(dir.path()),
        dir.path().join("config.json")
    );
    std::fs::write(dir.path().join("kilo.json"), "{}").unwrap();
    assert_eq!(
        kilo_user_config_file(dir.path()),
        dir.path().join("kilo.json")
    );
}

#[test]
fn detect_copilot_and_cline_cli_file_names() {
    let homes = HomeDirs::detect();
    assert_eq!(
        homes.copilot_cli.file_name().and_then(|n| n.to_str()),
        Some("mcp-config.json")
    );
    assert_eq!(
        homes.copilot_marker.file_name().and_then(|n| n.to_str()),
        Some("config.json")
    );
    assert_eq!(homes.copilot_cli.parent(), homes.copilot_marker.parent());
}

#[test]
fn only_the_cline_and_roo_extension_rows_require_the_vscode_user_dir() {
    let cwd = Path::new("/tmp/proj");
    let homes = HomeDirs {
        vscode: PathBuf::from("/cfg/Code/User/mcp.json"),
        ..HomeDirs::default()
    };
    let specs = build_specs(None, IdeScope::All, false, cwd, &homes);
    for s in &specs {
        let expected = matches!(
            (s.kind, s.scope),
            (ClientKind::Cline, Scope::User) | (ClientKind::Roo, Scope::User)
        )
        .then(|| PathBuf::from("/cfg/Code/User"));
        assert_eq!(s.requires, expected, "{:?} {:?}", s.kind, s.scope);
    }
}
