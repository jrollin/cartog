//! Tests for config-to-runtime conversions.

use crate::config::*;
use std::fs;

#[test]
fn index_exclude_parses_valid_globs() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg_path = dir.path().join("config.toml");
    fs::write(
        &cfg_path,
        "[index]\nexclude = [\"mobile/ios/Pods/**\", \"**/*.md\"]\n",
    )
    .unwrap();
    let cfg = read_config(&cfg_path).expect("should parse");
    assert!(!to_walk_filter(&cfg).unwrap().exclude.is_empty());
}

#[test]
fn index_exclude_rejects_invalid_glob() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg_path = dir.path().join("config.toml");
    fs::write(&cfg_path, "[index]\nexclude = [\"[unclosed\"]\n").unwrap();
    assert!(
        read_config(&cfg_path).is_none(),
        "malformed glob must reject"
    );
}

#[test]
fn index_exclude_absent_is_empty() {
    let cfg = CartogConfig::default();
    let filter = to_walk_filter(&cfg).unwrap();
    assert!(filter.exclude.is_empty());
    assert!(filter.respect_gitignore, "default honors .gitignore");
}

#[test]
fn respect_gitignore_defaults_true_and_parses_false() {
    // Default (no key) → true.
    assert!(
        to_walk_filter(&CartogConfig::default())
            .unwrap()
            .respect_gitignore
    );
    // Explicit false parses through.
    let dir = tempfile::TempDir::new().unwrap();
    let cfg_path = dir.path().join("config.toml");
    fs::write(&cfg_path, "[index]\nrespect_gitignore = false\n").unwrap();
    let cfg = read_config(&cfg_path).expect("should parse");
    assert!(!to_walk_filter(&cfg).unwrap().respect_gitignore);
}

#[test]
fn resolve_jobs_precedence_env_over_toml_over_auto() {
    assert_eq!(resolve_jobs(None, None), 0, "neither set → 0 (auto)");
    assert_eq!(resolve_jobs(None, Some(4)), 4, "toml when env absent");
    assert_eq!(resolve_jobs(Some(2), Some(4)), 2, "env wins over toml");
    // env=0 is an explicit value that wins; 0 = auto (documented).
    assert_eq!(
        resolve_jobs(Some(0), Some(4)),
        0,
        "env 0 overrides toml → auto"
    );
}

#[test]
fn resolve_lsp_max_servers_precedence() {
    assert_eq!(resolve_lsp_max_servers(None, None), 0, "neither → 0 (auto)");
    assert_eq!(
        resolve_lsp_max_servers(None, Some(4)),
        4,
        "toml when env absent"
    );
    assert_eq!(
        resolve_lsp_max_servers(Some(2), Some(4)),
        2,
        "env wins over toml"
    );
}

#[test]
fn lsp_max_servers_coexists_with_lang_overrides() {
    // The flatten must route max_concurrent_servers to the named field, not
    // into langs, while [lsp.<lang>] still populates langs.
    let toml_str = "[lsp]\nmax_concurrent_servers = 2\n[lsp.rust]\ncommand = [\"x\"]\n";
    let cfg: CartogConfig = toml::from_str(toml_str).unwrap();
    let lsp = cfg.lsp.unwrap();
    assert_eq!(lsp.max_concurrent_servers, Some(2));
    assert!(lsp.langs.contains_key("rust"));
    assert!(
        !lsp.langs.contains_key("max_concurrent_servers"),
        "scalar not swept into langs"
    );
}

#[test]
fn resolve_embed_concurrency_precedence_and_clamp() {
    assert_eq!(resolve_embed_concurrency(None, None), 4, "default 4");
    assert_eq!(
        resolve_embed_concurrency(None, Some(8)),
        8,
        "toml when env absent"
    );
    assert_eq!(
        resolve_embed_concurrency(Some(2), Some(8)),
        2,
        "env wins over toml"
    );
    assert_eq!(
        resolve_embed_concurrency(Some(0), None),
        1,
        "clamped up to 1"
    );
    assert_eq!(
        resolve_embed_concurrency(Some(99), None),
        16,
        "clamped down to 16"
    );
}

#[test]
fn index_jobs_parses_from_toml() {
    // env-free: drives the TOML tier directly via resolve_jobs to avoid a
    // CARTOG_JOBS leak in to_walk_filter.
    let dir = tempfile::TempDir::new().unwrap();
    let cfg_path = dir.path().join("config.toml");
    fs::write(&cfg_path, "[index]\njobs = 4\n").unwrap();
    let cfg = read_config(&cfg_path).expect("should parse");
    assert_eq!(cfg.index.and_then(|i| i.jobs), Some(4));
}

#[test]
fn parse_usize_or_warn_accepts_integers_and_rejects_garbage() {
    assert_eq!(parse_usize_or_warn("X", "8"), Some(8));
    assert_eq!(parse_usize_or_warn("X", " 8 "), Some(8));
    assert_eq!(parse_usize_or_warn("X", "0"), Some(0));
    assert_eq!(parse_usize_or_warn("X", "abc"), None);
    assert_eq!(parse_usize_or_warn("X", "-1"), None);
    assert_eq!(parse_usize_or_warn("X", ""), None);
}

#[test]
fn test_to_provider_config_threads_openai_settings() {
    let toml_str = r#"
[embedding]
provider = "openai"

[embedding.openai]
base_url = "https://api.mistral.ai/v1"
model = "mistral-embed"
api_key_env = "MISTRAL_API_KEY"
"#;
    let cfg: CartogConfig = toml::from_str(toml_str).unwrap();
    let pc = to_provider_config(&cfg);
    assert_eq!(pc.provider, "openai");
    assert_eq!(pc.model.as_deref(), Some("mistral-embed"));
    assert_eq!(pc.base_url.as_deref(), Some("https://api.mistral.ai/v1"));
    assert_eq!(pc.api_key_env.as_deref(), Some("MISTRAL_API_KEY"));
}

#[test]
fn test_to_provider_config_openai_ignores_lingering_ollama_block() {
    // A leftover [embedding.ollama] block must not override an openai config:
    // resolution keys off the active provider, not sub-table presence.
    let toml_str = r#"
[embedding]
provider = "openai"

[embedding.openai]
base_url = "https://api.openai.com/v1"
model = "text-embedding-3-small"

[embedding.ollama]
base_url = "http://localhost:11434"
model = "nomic-embed-text"
"#;
    let cfg: CartogConfig = toml::from_str(toml_str).unwrap();
    let pc = to_provider_config(&cfg);
    assert_eq!(pc.provider, "openai");
    assert_eq!(pc.base_url.as_deref(), Some("https://api.openai.com/v1"));
    assert_eq!(pc.model.as_deref(), Some("text-embedding-3-small"));
}

#[test]
fn test_to_provider_config_ollama_ignores_lingering_openai_block() {
    let toml_str = r#"
[embedding]
provider = "ollama"

[embedding.ollama]
base_url = "http://gpu:11434"

[embedding.openai]
base_url = "https://api.openai.com/v1"
api_key_env = "OPENAI_API_KEY"
"#;
    let cfg: CartogConfig = toml::from_str(toml_str).unwrap();
    let pc = to_provider_config(&cfg);
    assert_eq!(pc.provider, "ollama");
    assert_eq!(pc.base_url.as_deref(), Some("http://gpu:11434"));
    // api_key_env belongs to openai only — never leaks into an ollama config.
    assert!(pc.api_key_env.is_none());
}

#[test]
fn test_to_provider_config_reranker_model_from_toml() {
    let toml_str = r#"
[reranker]
model = "BAAI/bge-reranker-base"
"#;
    let cfg: CartogConfig = toml::from_str(toml_str).unwrap();
    let pc = to_provider_config(&cfg);
    assert_eq!(pc.reranker_model.as_deref(), Some("BAAI/bge-reranker-base"));
}

#[test]
fn test_to_provider_config_reranker_model_default_is_none() {
    // Unset model maps to None; cartog-rag resolves None to DEFAULT_RERANKER_MODEL.
    let cfg = CartogConfig::default();
    let pc = to_provider_config(&cfg);
    assert!(pc.reranker_model.is_none());
}

#[test]
fn test_reranker_provider_none_keeps_model_inert() {
    // A model alongside provider="none" parses fine; the model is simply never loaded.
    let toml_str = r#"
[reranker]
provider = "none"
model = "BAAI/bge-reranker-base"
"#;
    let cfg: CartogConfig = toml::from_str(toml_str).unwrap();
    let pc = to_provider_config(&cfg);
    assert_eq!(pc.reranker_provider, "none");
    assert_eq!(pc.reranker_model.as_deref(), Some("BAAI/bge-reranker-base"));
}

#[test]
fn test_to_provider_config_defaults() {
    let cfg = CartogConfig::default();
    let pc = to_provider_config(&cfg);
    assert_eq!(pc.provider, "local");
    assert!(pc.model.is_none());
    assert_eq!(pc.resolved_dimension(), 384);
    assert!(pc.query_prefix.is_none());
    assert!(pc.document_prefix.is_none());
}

#[test]
fn test_to_provider_config_from_toml() {
    let toml_str = r#"
[embedding]
provider = "ollama"
model = "nomic-embed-text"
dimension = 768

[embedding.local]
query_prefix = "search_query: "
document_prefix = "search_document: "
"#;
    let cfg: CartogConfig = toml::from_str(toml_str).unwrap();
    let pc = to_provider_config(&cfg);
    assert_eq!(pc.provider, "ollama");
    assert_eq!(pc.model.as_deref(), Some("nomic-embed-text"));
    assert_eq!(pc.resolved_dimension(), 768);
    assert_eq!(pc.query_prefix.as_deref(), Some("search_query: "));
    assert_eq!(pc.document_prefix.as_deref(), Some("search_document: "));
}

#[test]
fn test_provider_config_dimension_override() {
    let pc = cartog_rag::EmbeddingProviderConfig {
        dimension: Some(1536),
        ..Default::default()
    };
    assert_eq!(pc.resolved_dimension(), 1536);
}

#[test]
fn test_provider_config_dimension_default_fallback() {
    let pc = cartog_rag::EmbeddingProviderConfig::default();
    assert_eq!(pc.resolved_dimension(), 384);
    assert!(pc.dimension.is_none());
}

#[test]
fn test_to_provider_config_ollama_model_fallback() {
    let toml_str = r#"
[embedding]
provider = "ollama"

[embedding.ollama]
model = "mxbai-embed-large"
base_url = "http://gpu:11434"
"#;
    let cfg: CartogConfig = toml::from_str(toml_str).unwrap();
    let pc = to_provider_config(&cfg);
    assert_eq!(pc.provider, "ollama");
    assert_eq!(pc.model.as_deref(), Some("mxbai-embed-large"));
    assert_eq!(pc.base_url.as_deref(), Some("http://gpu:11434"));
}

#[test]
fn test_to_provider_config_top_level_model_wins() {
    let toml_str = r#"
[embedding]
provider = "ollama"
model = "top-level-model"

[embedding.ollama]
model = "ollama-model"
"#;
    let cfg: CartogConfig = toml::from_str(toml_str).unwrap();
    let pc = to_provider_config(&cfg);
    assert_eq!(pc.model.as_deref(), Some("top-level-model"),);
}

#[test]
fn test_to_provider_config_base_url_threaded() {
    let toml_str = r#"
[embedding]
provider = "ollama"

[embedding.ollama]
base_url = "http://custom:11434"
"#;
    let cfg: CartogConfig = toml::from_str(toml_str).unwrap();
    let pc = to_provider_config(&cfg);
    assert_eq!(pc.base_url.as_deref(), Some("http://custom:11434"));
}

#[test]
fn test_to_provider_config_no_base_url_when_local() {
    let toml_str = r#"
[embedding]
provider = "local"
"#;
    let cfg: CartogConfig = toml::from_str(toml_str).unwrap();
    let pc = to_provider_config(&cfg);
    assert!(pc.base_url.is_none());
}

#[test]
fn auto_embed_env_beats_config_and_flag() {
    assert_eq!(
        resolve_auto_embed_with(Some("0"), Some(true), true),
        Some(false)
    );
    assert_eq!(
        resolve_auto_embed_with(Some("on"), Some(false), false),
        Some(true)
    );
}

#[test]
fn auto_embed_config_beats_flag() {
    assert_eq!(
        resolve_auto_embed_with(None, Some(false), true),
        Some(false)
    );
    assert_eq!(resolve_auto_embed_with(None, Some(true), false), Some(true));
}

#[test]
fn auto_embed_flag_only_when_no_other_signal() {
    assert_eq!(resolve_auto_embed_with(None, None, true), Some(true));
}

#[test]
fn auto_embed_none_when_no_signal_defers_to_watcher() {
    assert_eq!(resolve_auto_embed_with(None, None, false), None);
    // Unparseable env falls through to the next tier.
    assert_eq!(resolve_auto_embed_with(Some("maybe"), None, false), None);
}
