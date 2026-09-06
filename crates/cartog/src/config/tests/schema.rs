//! Tests for config schema types and their defaults/accessors.

use crate::config::*;
use std::fs;

#[test]
fn redact_secrets_defaults_true_when_absent() {
    let cfg = CartogConfig::default();
    assert!(to_redaction_config(&cfg).enabled);
}

#[test]
fn redact_secrets_can_be_disabled_via_config() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg_path = dir.path().join("config.toml");
    fs::write(&cfg_path, "[security]\nredact_secrets = false\n").unwrap();
    let cfg = read_config(&cfg_path).expect("should parse");
    assert!(!to_redaction_config(&cfg).enabled);
}

#[test]
fn test_remote_config_valid_minimal() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg_path = dir.path().join(".cartog.toml");
    fs::write(
        &cfg_path,
        r#"[remote]
url = "s3://team-bucket/cartog/main"
region = "us-east-1"
"#,
    )
    .unwrap();
    let cfg = read_config(&cfg_path).expect("should parse");
    let remote = cfg.remote.expect("remote section parsed");
    assert_eq!(remote.url.as_deref(), Some("s3://team-bucket/cartog/main"));
    assert_eq!(remote.region.as_deref(), Some("us-east-1"));
    assert_eq!(remote.path_style, None);
}

#[test]
fn test_remote_config_full_minio_shape() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg_path = dir.path().join(".cartog.toml");
    fs::write(
        &cfg_path,
        r#"[remote]
url = "s3://b/k"
region = "us-east-1"
endpoint = "https://minio.local"
path_style = true
"#,
    )
    .unwrap();
    let cfg = read_config(&cfg_path).expect("should parse");
    let r = cfg.remote.unwrap();
    assert_eq!(r.endpoint.as_deref(), Some("https://minio.local"));
    assert_eq!(r.path_style, Some(true));
}

#[test]
fn test_remote_config_rejects_credential_keys() {
    for bad in [
        "access_key = \"AKIA...\"",
        "secret_key = \"...\"",
        "credentials = \"...\"",
        "token = \"...\"",
        "session_token = \"...\"",
        "password = \"...\"",
        "aws_access_key_id = \"...\"",
        "AWS_SECRET = \"...\"",
    ] {
        let dir = tempfile::TempDir::new().unwrap();
        let cfg_path = dir.path().join(".cartog.toml");
        fs::write(&cfg_path, format!("[remote]\nurl = \"s3://b/k\"\n{bad}\n")).unwrap();
        assert!(
            read_config(&cfg_path).is_none(),
            "should reject credential key: {bad}"
        );
    }
}

#[test]
fn test_remote_config_rejects_nested_credential_keys() {
    for bad_section in [
        "[remote.aws]\naccess_key = \"AKIA...\"\n",
        "[remote.creds]\nsecret_key = \"...\"\n",
        "[remote.minio]\naws_session_token = \"...\"\n",
    ] {
        let dir = tempfile::TempDir::new().unwrap();
        let cfg_path = dir.path().join(".cartog.toml");
        fs::write(
            &cfg_path,
            format!("[remote]\nurl = \"s3://b/k\"\n{bad_section}"),
        )
        .unwrap();
        assert!(
            read_config(&cfg_path).is_none(),
            "should reject nested credential: {bad_section}"
        );
    }
}

#[test]
fn test_remote_config_rejects_credential_prefixes() {
    for bad in [
        // Names not in CREDENTIAL_KEYS but caught by prefix.
        "access_token_v2 = \"...\"",
        "secret_value = \"...\"",
        "aws_role_arn = \"...\"",
    ] {
        let dir = tempfile::TempDir::new().unwrap();
        let cfg_path = dir.path().join(".cartog.toml");
        fs::write(&cfg_path, format!("[remote]\nurl = \"s3://b/k\"\n{bad}\n")).unwrap();
        assert!(
            read_config(&cfg_path).is_none(),
            "should reject prefix-matched credential key: {bad}"
        );
    }
}

#[test]
fn test_remote_config_rejects_unknown_field() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg_path = dir.path().join(".cartog.toml");
    fs::write(
        &cfg_path,
        "[remote]\nurl = \"s3://b/k\"\npathstyle = true\n",
    )
    .unwrap();
    assert!(read_config(&cfg_path).is_none());
}

#[test]
fn test_remote_config_rejects_endpoint_with_userinfo() {
    for bad in [
        "http://AKIA:secret@minio.local",
        "https://user@s3.example.com",
        // No scheme — still parseable as userinfo + host.
        "AKIA:secret@host:9000",
        "https://user:pass@host/path",
    ] {
        let dir = tempfile::TempDir::new().unwrap();
        let cfg_path = dir.path().join(".cartog.toml");
        fs::write(
            &cfg_path,
            format!("[remote]\nurl = \"s3://b/k\"\nendpoint = \"{bad}\"\n"),
        )
        .unwrap();
        assert!(
            read_config(&cfg_path).is_none(),
            "should reject endpoint with userinfo: {bad}"
        );
    }
}

#[test]
fn test_remote_config_accepts_clean_endpoints() {
    for ok in [
        "https://s3.us-east-1.amazonaws.com",
        "https://minio.example.com:9000",
        "https://r2.cloudflarestorage.com/path",
        "http://localhost:4566",
    ] {
        let dir = tempfile::TempDir::new().unwrap();
        let cfg_path = dir.path().join(".cartog.toml");
        fs::write(
            &cfg_path,
            format!("[remote]\nurl = \"s3://b/k\"\nendpoint = \"{ok}\"\n"),
        )
        .unwrap();
        assert!(
            read_config(&cfg_path).is_some(),
            "should accept clean endpoint: {ok}"
        );
    }
}

#[test]
fn test_embedding_config_defaults() {
    let cfg = EmbeddingConfig::default();
    assert_eq!(cfg.provider(), "local");
    assert!(cfg.dimension.is_none());
    assert!(cfg.model.is_none());
    assert!(cfg.local.is_none());
    assert!(cfg.ollama.is_none());
}

#[test]
fn test_embedding_config_from_toml() {
    let toml_str = r#"
[embedding]
provider = "ollama"
model = "nomic-embed-text"
dimension = 768
"#;
    let cfg: CartogConfig = toml::from_str(toml_str).unwrap();
    let embed = cfg.embedding.unwrap();
    assert_eq!(embed.provider(), "ollama");
    assert_eq!(embed.model.as_deref(), Some("nomic-embed-text"));
    assert_eq!(embed.dimension, Some(768));
}

#[test]
fn test_embedding_config_local_with_prefixes() {
    let toml_str = r#"
[embedding]
provider = "local"
model = "BAAI/bge-small-en-v1.5"

[embedding.local]
query_prefix = "search_query: "
document_prefix = "search_document: "
"#;
    let cfg: CartogConfig = toml::from_str(toml_str).unwrap();
    let embed = cfg.embedding.unwrap();
    assert_eq!(embed.provider(), "local");
    let local = embed.local.unwrap();
    assert_eq!(local.query_prefix.as_deref(), Some("search_query: "));
    assert_eq!(local.document_prefix.as_deref(), Some("search_document: "));
}

#[test]
fn test_ollama_config_defaults() {
    let cfg = OllamaConfig::default();
    assert_eq!(cfg.base_url(), "http://localhost:11434");
    assert_eq!(cfg.model(), "nomic-embed-text");
}

#[test]
fn test_ollama_config_from_toml() {
    let toml_str = r#"
[embedding.ollama]
base_url = "http://gpu-server:11434"
model = "mxbai-embed-large"
"#;
    let cfg: CartogConfig = toml::from_str(toml_str).unwrap();
    let ollama = cfg.embedding.unwrap().ollama.unwrap();
    assert_eq!(ollama.base_url(), "http://gpu-server:11434");
    assert_eq!(ollama.model(), "mxbai-embed-large");
}

#[test]
fn test_openai_config_defaults() {
    let cfg = OpenAiConfig::default();
    assert_eq!(cfg.base_url(), "https://api.openai.com/v1");
    assert_eq!(cfg.model(), "text-embedding-3-small");
    assert_eq!(cfg.api_key_env(), "OPENAI_API_KEY");
}

#[test]
fn test_openai_config_from_toml() {
    let toml_str = r#"
[embedding.openai]
base_url = "http://localhost:11434/v1"
model = "nomic-embed-text"
api_key_env = "MY_OPENAI_KEY"
"#;
    let cfg: CartogConfig = toml::from_str(toml_str).unwrap();
    let openai = cfg.embedding.unwrap().openai.unwrap();
    assert_eq!(openai.base_url(), "http://localhost:11434/v1");
    assert_eq!(openai.model(), "nomic-embed-text");
    assert_eq!(openai.api_key_env(), "MY_OPENAI_KEY");
}

#[test]
fn test_reranker_config_defaults() {
    let cfg = RerankerConfig::default();
    assert_eq!(cfg.provider(), "local");
}

#[test]
fn test_reranker_config_none() {
    let toml_str = r#"
[reranker]
provider = "none"
"#;
    let cfg: CartogConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(cfg.reranker.unwrap().provider(), "none");
}

#[test]
fn test_reranker_model_defaults_to_none_in_config() {
    let cfg = RerankerConfig::default();
    assert!(cfg.model.is_none());
}

#[test]
fn test_full_config_backward_compat() {
    let toml_str = r#"
[database]
path = "/tmp/test.db"
"#;
    let cfg: CartogConfig = toml::from_str(toml_str).unwrap();
    assert!(cfg.embedding.is_none());
    assert!(cfg.reranker.is_none());
    assert_eq!(cfg.database.unwrap().path.as_deref(), Some("/tmp/test.db"));
}

#[test]
fn unknown_key_in_a_section_is_rejected() {
    // Previously ignored in silence, which is how `[reranker] enabled` shipped
    // in the init template for releases without being a real field.
    let toml_str = r#"
[embedding]
provider = "local"
unknown_field = "typo"
"#;
    let err = toml::from_str::<CartogConfig>(toml_str)
        .expect_err("unknown key must be rejected")
        .to_string();
    assert!(
        err.contains("unknown_field"),
        "error must name the key: {err}"
    );
}

#[test]
fn reranker_enabled_false_resolves_provider_to_none() {
    let cfg: CartogConfig = toml::from_str("[reranker]\nenabled = false\n").unwrap();
    assert_eq!(cfg.reranker.unwrap().provider(), "none");
}

#[test]
fn reranker_enabled_false_wins_over_explicit_provider() {
    let toml_str = r#"
[reranker]
enabled = false
provider = "local"
"#;
    let cfg: CartogConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(cfg.reranker.unwrap().provider(), "none");
}

#[test]
fn reranker_enabled_true_keeps_provider() {
    let toml_str = r#"
[reranker]
enabled = true
provider = "local"
"#;
    let cfg: CartogConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(cfg.reranker.unwrap().provider(), "local");
}

#[test]
fn to_search_tuning_clamps_zero_retrieval() {
    let cfg = RagConfig {
        retrieval_multiplier: Some(0),
        retrieval_floor: Some(0),
        rerank_max: None,
        rerank_min: None,
    };
    let t = cfg.to_search_tuning();
    assert_eq!(t.retrieval_multiplier, 1);
    assert_eq!(t.retrieval_floor, 1);
}

#[test]
fn to_search_tuning_caps_rerank_min_at_max() {
    let cfg = RagConfig {
        retrieval_multiplier: None,
        retrieval_floor: None,
        rerank_max: Some(10),
        rerank_min: Some(50),
    };
    let t = cfg.to_search_tuning();
    assert_eq!(t.rerank_max, 10);
    assert_eq!(t.rerank_min, 10);
}

#[test]
fn to_search_tuning_passes_valid_values() {
    let cfg = RagConfig {
        retrieval_multiplier: Some(5),
        retrieval_floor: Some(40),
        rerank_max: Some(100),
        rerank_min: Some(10),
    };
    let t = cfg.to_search_tuning();
    assert_eq!(t.retrieval_multiplier, 5);
    assert_eq!(t.retrieval_floor, 40);
    assert_eq!(t.rerank_max, 100);
    assert_eq!(t.rerank_min, 10);
}

// ── `[project]` ─────────────────────────────────────────────────────────────

/// A section with no keys is valid and inert — every sibling section behaves
/// the same way, so a user can write the header before deciding what to say.
#[test]
fn bare_project_header_parses_with_no_name_or_description() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg_path = dir.path().join(".cartog.toml");
    fs::write(&cfg_path, "[project]\n").unwrap();

    let cfg = read_config(&cfg_path).expect("bare [project] must parse");

    let project = cfg.project.expect("section present");
    assert_eq!(project.name(), None);
    assert_eq!(project.description(), None);
}

#[test]
fn project_name_and_description_round_trip() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg_path = dir.path().join(".cartog.toml");
    fs::write(
        &cfg_path,
        "[project]\nname = \"svc-billing\"\ndescription = \"Invoice generation.\"\n",
    )
    .unwrap();

    let cfg = read_config(&cfg_path).expect("should parse");

    let project = cfg.project.unwrap();
    assert_eq!(project.name(), Some("svc-billing"));
    assert_eq!(project.description(), Some("Invoice generation."));
}

/// Accessors trim, so a value padded in TOML reaches the registry clean.
#[test]
fn project_accessors_trim_surrounding_whitespace() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg_path = dir.path().join(".cartog.toml");
    fs::write(
        &cfg_path,
        "[project]\nname = \"  svc-billing \"\ndescription = \"  Invoices.  \"\n",
    )
    .unwrap();

    let cfg = read_config(&cfg_path).expect("should parse");

    let project = cfg.project.unwrap();
    assert_eq!(project.name(), Some("svc-billing"));
    assert_eq!(project.description(), Some("Invoices."));
}

/// `deny_unknown_fields` is what turns `descriptoin = "..."` into an actionable
/// named warning instead of a mystery empty description. Without it the key is
/// silently ignored and the salvage path (which produces the same final config)
/// can't tell the difference — so the raw parse is what pins it.
#[test]
fn unknown_key_in_project_is_named_by_the_parser() {
    let err = toml::from_str::<CartogConfig>("[project]\ndescriptoin = \"typo\"\n")
        .expect_err("an unknown [project] key must be rejected by the parser")
        .to_string();

    assert!(
        err.contains("descriptoin"),
        "error must name the key: {err}"
    );
}

/// The registry hard-caps a stored description at the same number. A drift
/// between the two would either reject values the registry accepts or store
/// values the config swore were short enough. The cross-crate equality assert
/// lives with the integration work; this pins the config side's literal.
#[test]
fn project_description_cap_is_280_chars() {
    assert_eq!(PROJECT_DESCRIPTION_MAX_CHARS, 280);
    assert_eq!(PROJECT_NAME_MAX_CHARS, 100);
}

// ── `[mcp]` ─────────────────────────────────────────────────────────────────

/// Off unless a project says otherwise: the two cross-project tools read other
/// repositories' paths and README text into the session.
#[test]
fn mcp_federated_defaults_false_when_absent() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg_path = dir.path().join(".cartog.toml");
    fs::write(&cfg_path, "[mcp]\n").unwrap();

    let cfg = read_config(&cfg_path).expect("bare [mcp] must parse");

    assert!(!cfg.mcp.expect("section present").federated());
}

#[test]
fn mcp_federated_true_is_read_from_config() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg_path = dir.path().join(".cartog.toml");
    fs::write(&cfg_path, "[mcp]\nfederated = true\n").unwrap();

    let cfg = read_config(&cfg_path).expect("[mcp] must parse");

    assert!(cfg.mcp.expect("section present").federated());
}

/// A `federatd` typo is **salvaged**, not fatal: the file still loads and every
/// other section still applies. The cost is that the tools stay hidden, which
/// is the safe direction — but it is silent in the load result, so the warning
/// `read_config` prints is the only signal the user gets.
///
/// Asserted through `read_config`, not the raw parser: production catches the
/// `deny_unknown_fields` error and re-parses, so a parser-level assertion would
/// pin an error string that never reaches a user.
#[test]
fn unknown_key_in_mcp_is_salvaged_and_leaves_the_tools_hidden() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg_path = dir.path().join(".cartog.toml");
    fs::write(&cfg_path, "[mcp]\nfederatd = true\n").unwrap();

    let cfg = read_config(&cfg_path).expect("a stray [mcp] key must salvage, not reject");

    assert!(
        !cfg.mcp.unwrap_or_default().federated(),
        "a typo'd key must not enable the cross-project tools"
    );
}
