//! Tests for config loading, validation, and db-path resolution.

use crate::config::*;
use serial_test::serial;
use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn test_expand_tilde_with_home() {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| "/tmp".into());
    let expanded = expand_tilde(PathBuf::from("~/foo/bar"));
    assert_eq!(expanded, PathBuf::from(home).join("foo/bar"));
}

#[test]
fn unknown_sections_flags_typos_but_not_known_keys() {
    let raw: toml::value::Table =
        toml::from_str("[embeddings]\nprovider = \"ollama\"\n[database]\npath = \"x\"\n").unwrap();
    let unknown = unknown_sections(&raw);
    assert_eq!(unknown, vec!["embeddings"]);
}

#[test]
fn unknown_sections_empty_for_all_known() {
    let raw: toml::value::Table = toml::from_str(
        "[database]\npath = \"x\"\n[embedding]\nprovider = \"local\"\n[index]\nexclude = []\n",
    )
    .unwrap();
    assert!(unknown_sections(&raw).is_empty());
}

#[test]
fn validate_providers_accepts_known_values() {
    let config: CartogConfig =
        toml::from_str("[embedding]\nprovider = \"ollama\"\n[reranker]\nprovider = \"none\"\n")
            .unwrap();
    assert!(validate_providers(&config).is_ok());
}

#[test]
fn validate_providers_accepts_absent_provider() {
    let config = CartogConfig::default();
    assert!(validate_providers(&config).is_ok());
}

#[test]
fn validate_providers_rejects_unknown_embedding_provider() {
    let config: CartogConfig = toml::from_str("[embedding]\nprovider = \"ollma\"\n").unwrap();
    let err = validate_providers(&config).unwrap_err();
    assert!(
        err.contains("ollma"),
        "error should name the bad value: {err}"
    );
}

#[test]
fn validate_providers_rejects_unknown_reranker_provider() {
    let config: CartogConfig = toml::from_str("[reranker]\nprovider = \"bogus\"\n").unwrap();
    let err = validate_providers(&config).unwrap_err();
    assert!(
        err.contains("bogus"),
        "error should name the bad value: {err}"
    );
}

#[test]
fn read_config_rejects_unknown_provider() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg_path = dir.path().join("config.toml");
    fs::write(&cfg_path, "[embedding]\nprovider = \"ollma\"\n").unwrap();
    assert!(read_config(&cfg_path).is_none());
}

#[test]
fn test_expand_tilde_no_tilde() {
    let p = PathBuf::from("/absolute/path");
    assert_eq!(expand_tilde(p.clone()), p);
}

#[test]
fn test_read_config_valid_toml() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg_path = dir.path().join("config.toml");
    fs::write(&cfg_path, "[database]\npath = \"/tmp/test.db\"\n").unwrap();
    let cfg = read_config(&cfg_path).expect("should parse");
    assert_eq!(
        cfg.database.as_ref().unwrap().path.as_deref(),
        Some("/tmp/test.db")
    );
}

#[test]
fn test_read_config_missing_file_returns_none() {
    let result = read_config(Path::new("/nonexistent/path/config.toml"));
    assert!(result.is_none());
}

#[test]
fn test_read_config_invalid_toml_returns_none() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg_path = dir.path().join("config.toml");
    fs::write(&cfg_path, "this is {{ not valid toml").unwrap();
    assert!(read_config(&cfg_path).is_none());
}

#[test]
fn test_read_config_empty_toml_returns_default() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg_path = dir.path().join("config.toml");
    fs::write(&cfg_path, "").unwrap();
    let cfg = read_config(&cfg_path).expect("empty toml is valid");
    assert!(cfg.database.is_none());
}

#[test]
fn test_resolve_explicit_wins_over_config() {
    let cfg = CartogConfig {
        database: Some(DatabaseConfig {
            path: Some("/config/path.db".to_string()),
        }),
        ..Default::default()
    };
    let result = resolve_db_path(Some(PathBuf::from("/explicit/path.db")), &cfg);
    assert_eq!(result, PathBuf::from("/explicit/path.db"));
}

#[test]
fn test_resolve_config_path_used_when_no_explicit() {
    let cfg = CartogConfig {
        database: Some(DatabaseConfig {
            path: Some("/config/proj.db".to_string()),
        }),
        ..Default::default()
    };
    let result = resolve_db_path(None, &cfg);
    assert_eq!(result, PathBuf::from("/config/proj.db"));
}

#[test]
#[serial]
fn test_resolve_fallback_when_no_config_and_no_git() {
    let dir = tempfile::TempDir::new().unwrap();
    let canonical = dir.path().canonicalize().unwrap();
    let original = std::env::current_dir().unwrap();
    std::env::set_current_dir(dir.path()).unwrap();

    let result = resolve_db_path(None, &CartogConfig::default());
    std::env::set_current_dir(original).unwrap();

    assert_eq!(
        result,
        canonical
            .join(cartog_db::DB_DIR)
            .join(cartog_db::DB_FILENAME)
    );
}

#[test]
#[serial]
fn test_resolve_git_root_detection() {
    let dir = tempfile::TempDir::new().unwrap();
    let canonical_root = dir.path().canonicalize().unwrap();
    let git_dir = dir.path().join(".git");
    std::fs::create_dir(&git_dir).unwrap();
    let subdir = dir.path().join("subdir");
    std::fs::create_dir(&subdir).unwrap();

    let original = std::env::current_dir().unwrap();
    std::env::set_current_dir(&subdir).unwrap();

    let result = resolve_db_path(None, &CartogConfig::default());
    std::env::set_current_dir(original).unwrap();

    assert_eq!(
        result,
        canonical_root
            .join(cartog_db::DB_DIR)
            .join(cartog_db::DB_FILENAME)
    );
}

#[test]
#[serial]
fn test_resolve_prefers_new_layout_over_legacy() {
    let dir = tempfile::TempDir::new().unwrap();
    let canonical_root = dir.path().canonicalize().unwrap();
    std::fs::create_dir(dir.path().join(".git")).unwrap();
    // Both files exist — new layout wins.
    std::fs::create_dir(dir.path().join(cartog_db::DB_DIR)).unwrap();
    std::fs::write(
        dir.path()
            .join(cartog_db::DB_DIR)
            .join(cartog_db::DB_FILENAME),
        b"",
    )
    .unwrap();
    std::fs::write(dir.path().join(cartog_db::LEGACY_DB_FILE), b"").unwrap();

    let original = std::env::current_dir().unwrap();
    std::env::set_current_dir(dir.path()).unwrap();
    let result = resolve_db_path(None, &CartogConfig::default());
    std::env::set_current_dir(original).unwrap();

    assert_eq!(
        result,
        canonical_root
            .join(cartog_db::DB_DIR)
            .join(cartog_db::DB_FILENAME)
    );
}

#[test]
#[serial]
fn test_resolve_falls_back_to_legacy_db_file() {
    let dir = tempfile::TempDir::new().unwrap();
    let canonical_root = dir.path().canonicalize().unwrap();
    std::fs::create_dir(dir.path().join(".git")).unwrap();
    // Only legacy file exists — picks it up (and warns once).
    std::fs::write(dir.path().join(cartog_db::LEGACY_DB_FILE), b"").unwrap();

    let original = std::env::current_dir().unwrap();
    std::env::set_current_dir(dir.path()).unwrap();
    let result = resolve_db_path(None, &CartogConfig::default());
    std::env::set_current_dir(original).unwrap();

    assert_eq!(result, canonical_root.join(cartog_db::LEGACY_DB_FILE));
}

#[test]
fn validate_providers_accepts_openai() {
    let config: CartogConfig = toml::from_str("[embedding]\nprovider = \"openai\"\n").unwrap();
    assert!(validate_providers(&config).is_ok());
}

#[test]
fn lsp_override_parses_nested_table() {
    let toml_str = r#"
[lsp.dart]
command = ["docker", "run", "--rm", "-i", "-v", "${ROOT}:${ROOT}", "cartog-lsp-dart:stable"]
"#;
    let cfg: CartogConfig = toml::from_str(toml_str).unwrap();
    let dart = &cfg.lsp.unwrap().langs["dart"];
    assert_eq!(dart.command[0], "docker");
    assert_eq!(dart.command.last().unwrap(), "cartog-lsp-dart:stable");
}

#[test]
fn to_lsp_overrides_flattens_to_argv_map() {
    let toml_str = r#"
[lsp.go]
command = ["gopls", "serve"]
"#;
    let cfg: CartogConfig = toml::from_str(toml_str).unwrap();
    let map = to_lsp_overrides(&cfg);
    assert_eq!(map["go"], vec!["gopls".to_string(), "serve".to_string()]);
}

#[test]
fn to_lsp_overrides_empty_when_absent() {
    assert!(to_lsp_overrides(&CartogConfig::default()).is_empty());
}

#[test]
fn read_config_rejects_empty_lsp_command() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg_path = dir.path().join(".cartog.toml");
    fs::write(&cfg_path, "[lsp.dart]\ncommand = []\n").unwrap();
    assert!(read_config(&cfg_path).is_none());
}

#[test]
fn read_config_rejects_unknown_lsp_field() {
    // deny_unknown_fields on LspLangConfig: a typo like `cmd` must fail.
    let dir = tempfile::TempDir::new().unwrap();
    let cfg_path = dir.path().join(".cartog.toml");
    fs::write(&cfg_path, "[lsp.dart]\ncmd = [\"x\"]\n").unwrap();
    assert!(read_config(&cfg_path).is_none());
}

#[test]
fn read_config_accepts_valid_lsp_block() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg_path = dir.path().join(".cartog.toml");
    fs::write(&cfg_path, "[lsp.go]\ncommand = [\"gopls\", \"serve\"]\n").unwrap();
    let cfg = read_config(&cfg_path).expect("valid lsp block parses");
    assert!(cfg.lsp.unwrap().langs.contains_key("go"));
}

#[cfg(feature = "lsp")]
#[test]
fn read_config_rejects_unknown_lsp_language() {
    // A typo like `[lsp.pytho]` must fail at config load, not at first LSP use.
    let dir = tempfile::TempDir::new().unwrap();
    let cfg_path = dir.path().join(".cartog.toml");
    fs::write(&cfg_path, "[lsp.pytho]\ncommand = [\"x\"]\n").unwrap();
    assert!(read_config(&cfg_path).is_none());
}
