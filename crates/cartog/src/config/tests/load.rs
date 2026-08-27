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

    // `cmd` alone is a *missing field* error, which would pass even with
    // `deny_unknown_fields` off. Pair a valid `command` with a stray key so this
    // actually exercises unknown-field rejection.
    let stray = dir.path().join("stray.toml");
    fs::write(&stray, "[lsp.dart]\ncommand = [\"x\"]\nargz = 1\n").unwrap();
    assert!(
        read_config(&stray).is_none(),
        "a stray key alongside a valid `command` must still reject"
    );
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

// ── Consent gate predicate ──

#[test]
#[serial]
fn allow_index_creation_refuses_fresh_repo() {
    let dir = tempfile::TempDir::new().unwrap();
    let absent = dir.path().join(".cartog").join("db.sqlite");
    let _guard = scopeguard(AUTO_INIT_ENV);
    std::env::remove_var(AUTO_INIT_ENV);
    assert!(
        !allow_index_creation(&absent, false),
        "no config + no DB + no env must refuse"
    );
}

#[test]
fn allow_index_creation_allows_with_config_present() {
    let dir = tempfile::TempDir::new().unwrap();
    let absent = dir.path().join(".cartog").join("db.sqlite");
    assert!(allow_index_creation(&absent, true));
}

#[test]
fn allow_index_creation_allows_with_existing_db() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = dir.path().join("db.sqlite");
    std::fs::write(&db, b"").unwrap();
    assert!(allow_index_creation(&db, false));
}

#[test]
#[serial]
fn allow_index_creation_stray_wal_without_main_file_is_gated() {
    // Keyed on the main DB file; a stray -wal alone is still "fresh".
    let dir = tempfile::TempDir::new().unwrap();
    let db = dir.path().join("db.sqlite");
    std::fs::write(dir.path().join("db.sqlite-wal"), b"").unwrap();
    let _guard = scopeguard(AUTO_INIT_ENV);
    std::env::remove_var(AUTO_INIT_ENV);
    assert!(!allow_index_creation(&db, false));
}

#[test]
#[serial]
fn allow_index_creation_allows_with_auto_init_env() {
    let dir = tempfile::TempDir::new().unwrap();
    let absent = dir.path().join(".cartog").join("db.sqlite");
    let _guard = scopeguard(AUTO_INIT_ENV);
    std::env::set_var(AUTO_INIT_ENV, "1");
    assert!(allow_index_creation(&absent, false));
}

#[test]
#[serial]
fn allow_index_creation_ignores_empty_auto_init_env() {
    let dir = tempfile::TempDir::new().unwrap();
    let absent = dir.path().join(".cartog").join("db.sqlite");
    let _guard = scopeguard(AUTO_INIT_ENV);
    std::env::set_var(AUTO_INIT_ENV, "");
    assert!(
        !allow_index_creation(&absent, false),
        "an empty CARTOG_AUTO_INIT must not count as opt-in"
    );
}

/// Restore an env var to its pre-test value on drop, so a `set_var`/`remove_var`
/// in one `#[serial]` test can't leak into another.
fn scopeguard(key: &'static str) -> impl Drop {
    struct Restore {
        key: &'static str,
        prev: Option<String>,
    }
    impl Drop for Restore {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }
    Restore {
        key,
        prev: std::env::var(key).ok(),
    }
}

/// An unknown key is a typo, not a reason to discard the file. Before this was
/// handled, `deny_unknown_fields` made one misspelling drop every other setting
/// AND revoke index-creation consent (`config_present` is false for `Rejected`),
/// so `cartog index` refused with "no .cartog.toml in this project".
#[test]
fn unknown_key_keeps_the_rest_of_the_config() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg_path = dir.path().join(".cartog.toml");
    std::fs::write(
        &cfg_path,
        "[database]\npath = \"/tmp/kept.db\"\n\n[rag]\nrerank_mx = 10\nrerank_max = 33\n",
    )
    .unwrap();

    let cfg = read_config(&cfg_path).expect("a stray key must not reject the whole config");
    assert_eq!(
        cfg.database.expect("[database] survives").path.as_deref(),
        Some("/tmp/kept.db"),
    );
    // A valid sibling in the *same* section survives too.
    assert_eq!(cfg.rag.expect("[rag] survives").rerank_max, Some(33));
}

#[test]
fn unknown_key_still_loads_so_consent_is_preserved() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg_path = dir.path().join(".cartog.toml");
    std::fs::write(&cfg_path, "[security]\nredact_secretz = false\n").unwrap();
    // `Some` is what makes `ConfigLoad::Loaded` (= consent to create an index).
    assert!(
        read_config(&cfg_path).is_some(),
        "a typo must not revoke index-creation consent"
    );
}

#[test]
fn genuine_syntax_error_is_still_rejected() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg_path = dir.path().join(".cartog.toml");
    std::fs::write(&cfg_path, "[database\npath = \"x\"\n").unwrap();
    assert!(read_config(&cfg_path).is_none());
}

#[test]
fn wrong_value_type_is_still_rejected() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg_path = dir.path().join(".cartog.toml");
    std::fs::write(&cfg_path, "[index]\njobs = \"many\"\n").unwrap();
    assert!(read_config(&cfg_path).is_none());
}

#[test]
fn unknown_lsp_scalar_key_is_dropped_not_fatal() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg_path = dir.path().join(".cartog.toml");
    std::fs::write(
        &cfg_path,
        "[lsp]\nmax_concurrent_serverz = 2\n\n[lsp.rust]\ncommand = [\"rust-analyzer\"]\n",
    )
    .unwrap();
    let cfg = read_config(&cfg_path).expect("[lsp] typo must not reject the config");
    let lsp = cfg.lsp.expect("[lsp] survives");
    assert!(
        lsp.langs.contains_key("rust"),
        "per-language entry survives"
    );
    assert_eq!(lsp.max_concurrent_servers, None);
}

/// `[remote]` is exempt from the lenient unknown-key path: a mistyped key there
/// could silently redirect where the index is pushed or pulled.
#[test]
fn unknown_remote_key_stays_a_hard_rejection() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg_path = dir.path().join(".cartog.toml");
    std::fs::write(
        &cfg_path,
        "[remote]\nurl = \"s3://b/k\"\npathstyle = true\n",
    )
    .unwrap();
    assert!(
        read_config(&cfg_path).is_none(),
        "a stray [remote] key must reject the config, not be ignored"
    );
}

/// The lenient path keys off `toml`'s error text (`is_unknown_field_error`).
/// If a dep bump rewords it, typos silently revert to full rejection — which
/// also revokes index-creation consent. Fail loudly here instead.
#[test]
fn toml_still_reports_unknown_fields_in_the_format_we_parse() {
    let e = toml::from_str::<CartogConfig>("[security]\nredact_secretz = false\n")
        .expect_err("unknown field must error");
    assert!(
        e.to_string().contains("unknown field"),
        "toml error format changed — is_unknown_field_error is now dead: {e}"
    );
}

/// A stray key must be dropped from the section it was written in, never matched
/// by name across the tree. `[rag] path` once deleted `[database] path`, silently
/// pointing cartog at a different database.
#[test]
fn typo_does_not_delete_a_same_named_key_in_another_section() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg_path = dir.path().join(".cartog.toml");
    std::fs::write(
        &cfg_path,
        "[database]\npath = \"/tmp/kept.db\"\n\n[rag]\npath = \"oops\"\nrerank_max = 33\n",
    )
    .unwrap();
    let cfg = read_config(&cfg_path).expect("stray key must not reject the config");
    assert_eq!(
        cfg.database.expect("[database] survives").path.as_deref(),
        Some("/tmp/kept.db"),
        "a [rag] typo must not delete [database] path"
    );
    assert_eq!(cfg.rag.expect("[rag] survives").rerank_max, Some(33));
}

#[test]
fn typo_does_not_delete_embedding_provider_from_another_section() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg_path = dir.path().join(".cartog.toml");
    std::fs::write(
        &cfg_path,
        "[embedding]\nprovider = \"ollama\"\n\n[security]\nprovider = \"x\"\n",
    )
    .unwrap();
    let cfg = read_config(&cfg_path).expect("stray key must not reject the config");
    assert_eq!(
        cfg.embedding.expect("[embedding] survives").provider(),
        "ollama",
        "a [security] typo must not reset [embedding] provider to the default"
    );
}

/// Two typos in different sections: both siblings must survive. Exercises the
/// per-section pass more than once.
#[test]
fn multiple_typos_in_different_sections_all_resolve() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg_path = dir.path().join(".cartog.toml");
    std::fs::write(
        &cfg_path,
        "[database]\npath = \"/tmp/x.db\"\nbogus = 1\n\n[rag]\nrerank_max = 7\nalso_bogus = 2\n",
    )
    .unwrap();
    let cfg = read_config(&cfg_path).expect("stray keys must not reject the config");
    assert_eq!(
        cfg.database.expect("[database] survives").path.as_deref(),
        Some("/tmp/x.db")
    );
    assert_eq!(cfg.rag.expect("[rag] survives").rerank_max, Some(7));
}

/// `[lsp.<lang>]` stays strict: the argv there spawns a process, so a stray key
/// must reject rather than be silently dropped.
#[test]
fn unknown_lsp_lang_key_stays_a_hard_rejection() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg_path = dir.path().join(".cartog.toml");
    std::fs::write(
        &cfg_path,
        "[lsp.rust]\ncommand = [\"rust-analyzer\"]\nargz = 1\n",
    )
    .unwrap();
    assert!(
        read_config(&cfg_path).is_none(),
        "a stray [lsp.<lang>] key must reject the config"
    );
}

/// The `[remote]` boundary must not be defeatable from another section. A typo
/// named like a remote field once deleted the REAL `[remote] endpoint`, which
/// makes `cartog push` fall back to AWS's default host instead of the user's
/// private endpoint.
#[test]
fn typo_elsewhere_never_deletes_a_remote_key() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg_path = dir.path().join(".cartog.toml");
    std::fs::write(
        &cfg_path,
        "[remote]\nurl = \"s3://team/idx\"\nendpoint = \"https://minio.internal\"\n\
         \n[security]\nendpoint = \"typo\"\n",
    )
    .unwrap();
    let cfg = read_config(&cfg_path).expect("stray [security] key must not reject the config");
    let remote = cfg.remote.expect("[remote] survives");
    assert_eq!(
        remote.endpoint.as_deref(),
        Some("https://minio.internal"),
        "a typo in another section must never delete [remote] endpoint"
    );
    assert_eq!(remote.url.as_deref(), Some("s3://team/idx"));
}

/// `LSP_SCALAR_KEYS` hand-lists `LspConfig`'s non-flattened fields (it can't use
/// `deny_unknown_fields` — `#[serde(flatten)]` forbids it). If a new scalar field
/// is added to `LspConfig` and not to that list, it would be silently stripped as
/// a typo. Assert every listed key round-trips, so the two can't drift apart.
#[test]
fn lsp_scalar_keys_are_all_real_lsp_config_fields() {
    for key in LSP_SCALAR_KEYS {
        let dir = tempfile::TempDir::new().unwrap();
        let cfg_path = dir.path().join(".cartog.toml");
        // A usize-valued scalar covers today's only entry; extend if a
        // non-numeric scalar is ever added.
        std::fs::write(&cfg_path, format!("[lsp]\n{key} = 2\n")).unwrap();
        let cfg = read_config(&cfg_path)
            .unwrap_or_else(|| panic!("[lsp] {key} must parse — is it a real LspConfig field?"));
        let lsp = cfg
            .lsp
            .unwrap_or_else(|| panic!("[lsp] section must survive for key {key}"));
        assert!(
            !lsp.langs.contains_key(*key),
            "{key} was routed into the per-language map instead of a real field \
             — LSP_SCALAR_KEYS and LspConfig have drifted"
        );
    }
}

/// A stray key inside a provider sub-table must not take the sub-table with it.
/// Deleting `[embedding.openai]` silently moved the endpoint from a self-hosted
/// server to the public API and changed which env var supplies the key.
#[test]
fn typo_in_a_provider_subtable_keeps_the_subtable() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg_path = dir.path().join(".cartog.toml");
    std::fs::write(
        &cfg_path,
        "[embedding]\nprovider = \"openai\"\n\n[embedding.openai]\n\
         base_url = \"http://good.example/v1\"\napi_key_env = \"MY_CUSTOM_KEY\"\n\
         base_urll = \"typo\"\n",
    )
    .unwrap();
    let cfg = read_config(&cfg_path).expect("stray sub-table key must not reject the config");
    let openai = cfg
        .embedding
        .expect("[embedding] survives")
        .openai
        .expect("[embedding.openai] must survive a typo inside it");
    assert_eq!(
        openai.base_url(),
        "http://good.example/v1",
        "a typo must not repoint the endpoint at the public API"
    );
    assert_eq!(openai.api_key_env(), "MY_CUSTOM_KEY");
}
