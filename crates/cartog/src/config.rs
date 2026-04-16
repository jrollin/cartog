use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Top-level cartog configuration, loaded from `.cartog.toml`.
///
/// Priority (highest to lowest):
/// 1. `--db` CLI flag / `CARTOG_DB` env var  (handled in main)
/// 2. `.cartog.toml` at git root or cwd      (`database.path`)
/// 3. Auto git-root detection                (no config needed)
/// 4. cwd fallback
#[derive(Debug, Default, Deserialize)]
pub struct CartogConfig {
    pub database: Option<DatabaseConfig>,
    pub embedding: Option<EmbeddingConfig>,
    pub reranker: Option<RerankerConfig>,
    pub rag: Option<RagConfig>,
}

/// Tuning knobs for the hybrid search pipeline.
///
/// ```toml
/// [rag]
/// retrieval_multiplier = 3
/// retrieval_floor      = 20
/// rerank_max           = 50
/// rerank_min           = 8
/// ```
#[derive(Debug, Default, Clone, Deserialize)]
pub struct RagConfig {
    /// Over-retrieval multiplier for FTS5 + vector candidate pools.
    pub retrieval_multiplier: Option<u32>,
    /// Lower bound on candidate retrieval, independent of `limit`.
    pub retrieval_floor: Option<u32>,
    /// Cap on candidates passed to the cross-encoder.
    pub rerank_max: Option<u32>,
    /// Skip the cross-encoder entirely below this many candidates.
    pub rerank_min: Option<u32>,
}

impl RagConfig {
    /// Build a `SearchTuning` with caller-provided overrides applied on top
    /// of defaults, clamping invalid combinations so search never degrades
    /// to zero candidates or a silently-disabled reranker.
    pub fn to_search_tuning(&self) -> cartog_rag::search::SearchTuning {
        let d = cartog_rag::search::SearchTuning::default();
        // Multipliers and floors of 0 would collapse retrieval to nothing —
        // clamp to 1.
        let retrieval_multiplier = self
            .retrieval_multiplier
            .unwrap_or(d.retrieval_multiplier)
            .max(1);
        let retrieval_floor = self.retrieval_floor.unwrap_or(d.retrieval_floor).max(1);
        let rerank_max = self.rerank_max.unwrap_or(d.rerank_max);
        let rerank_min = self.rerank_min.unwrap_or(d.rerank_min);
        // If a user wrote `rerank_min > rerank_max`, the reranker would
        // silently never fire. Cap rerank_min at rerank_max.
        let rerank_min = rerank_min.min(rerank_max);
        cartog_rag::search::SearchTuning {
            retrieval_multiplier,
            retrieval_floor,
            rerank_max,
            rerank_min,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct DatabaseConfig {
    /// Filesystem path to the cartog SQLite database. Supports `~` expansion.
    pub path: Option<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct EmbeddingConfig {
    /// Provider type: "local" (default) or "ollama".
    pub provider: Option<String>,
    /// Model name. For "local": fastembed built-in name or HuggingFace repo ID.
    /// For "ollama": model name on the Ollama server.
    pub model: Option<String>,
    /// Embedding dimension. Auto-detected for built-in models, required for custom HF models.
    pub dimension: Option<usize>,
    /// Local provider settings (ONNX via fastembed).
    pub local: Option<LocalEmbeddingConfig>,
    /// Ollama provider settings.
    pub ollama: Option<OllamaConfig>,
}

pub const DEFAULT_EMBEDDING_PROVIDER: &str = "local";

impl EmbeddingConfig {
    pub fn provider(&self) -> &str {
        self.provider
            .as_deref()
            .unwrap_or(DEFAULT_EMBEDDING_PROVIDER)
    }
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct LocalEmbeddingConfig {
    /// Prefix prepended to text during search (e.g. "search_query: ").
    pub query_prefix: Option<String>,
    /// Prefix prepended to text during indexing (e.g. "search_document: ").
    pub document_prefix: Option<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct OllamaConfig {
    /// Ollama server URL (default: "http://localhost:11434").
    pub base_url: Option<String>,
    /// Model name (default: "nomic-embed-text").
    pub model: Option<String>,
}

pub const DEFAULT_OLLAMA_BASE_URL: &str = cartog_rag::providers::DEFAULT_OLLAMA_BASE_URL;
pub const DEFAULT_OLLAMA_MODEL: &str = cartog_rag::providers::DEFAULT_OLLAMA_MODEL;

impl OllamaConfig {
    pub fn base_url(&self) -> &str {
        self.base_url.as_deref().unwrap_or(DEFAULT_OLLAMA_BASE_URL)
    }

    pub fn model(&self) -> &str {
        self.model.as_deref().unwrap_or(DEFAULT_OLLAMA_MODEL)
    }
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct RerankerConfig {
    /// Provider type: "local" (default) or "none".
    pub provider: Option<String>,
}

pub const DEFAULT_RERANKER_PROVIDER: &str = "local";

impl RerankerConfig {
    pub fn provider(&self) -> &str {
        self.provider
            .as_deref()
            .unwrap_or(DEFAULT_RERANKER_PROVIDER)
    }
}

/// Convert the embedding config section into an `EmbeddingProviderConfig` for cartog-rag.
pub fn to_provider_config(config: &CartogConfig) -> cartog_rag::EmbeddingProviderConfig {
    match &config.embedding {
        Some(embed) => {
            let (query_prefix, document_prefix) = match &embed.local {
                Some(local) => (local.query_prefix.clone(), local.document_prefix.clone()),
                None => (None, None),
            };
            let ollama = embed.ollama.as_ref();
            cartog_rag::EmbeddingProviderConfig {
                provider: embed.provider().to_string(),
                model: embed
                    .model
                    .clone()
                    .or_else(|| ollama.map(|o| o.model().to_string())),
                dimension: embed.dimension,
                query_prefix,
                document_prefix,
                base_url: ollama.map(|o| o.base_url().to_string()),
                reranker_provider: config
                    .reranker
                    .as_ref()
                    .map(|r| r.provider().to_string())
                    .unwrap_or_else(|| DEFAULT_RERANKER_PROVIDER.to_string()),
            }
        }
        None => cartog_rag::EmbeddingProviderConfig::default(),
    }
}

/// Load the local project config from `.cartog.toml`.
/// Returns the parsed config and the path it was loaded from (if any).
pub fn load_config() -> (CartogConfig, Option<PathBuf>) {
    match local_config_path() {
        Some(p) => match read_config(&p) {
            Some(cfg) => (cfg, Some(p)),
            None => (CartogConfig::default(), None),
        },
        None => (CartogConfig::default(), None),
    }
}

/// Path to the local project config: `.cartog.toml` found by walking up from
/// cwd to the git root. Returns `None` if no such file exists.
fn local_config_path() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let candidate = dir.join(".cartog.toml");
        if candidate.exists() {
            return Some(candidate);
        }
        // Stop searching once we reach the git root without finding a config.
        if dir.join(".git").exists() {
            return None;
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

fn read_config(path: &Path) -> Option<CartogConfig> {
    let text = std::fs::read_to_string(path).ok()?;
    match toml::from_str::<CartogConfig>(&text) {
        Ok(cfg) => Some(cfg),
        Err(e) => {
            // Use eprintln rather than tracing — tracing may not be initialised yet.
            eprintln!("cartog: warning: failed to parse {}: {e}", path.display());
            None
        }
    }
}

/// Resolve the database path using the following priority:
///
/// 1. `explicit` — from `--db` flag or `CARTOG_DB` env var (already merged by clap)
/// 2. `config.database.path` — from `.cartog.toml` at git root / cwd
/// 3. Auto git-root detection — walk up from cwd to `.git`, place DB there
/// 4. cwd fallback — `.cartog.db` in the current directory
pub fn resolve_db_path(explicit: Option<PathBuf>, config: &CartogConfig) -> PathBuf {
    // 1. Explicit override (--db / CARTOG_DB)
    if let Some(p) = explicit {
        return expand_tilde(p);
    }

    // 2. Local project config
    if let Some(path_str) = config.database.as_ref().and_then(|d| d.path.as_deref()) {
        return expand_tilde(PathBuf::from(path_str));
    }

    // 3. Walk up to git root
    if let Ok(mut dir) = std::env::current_dir() {
        loop {
            if dir.join(".git").exists() {
                return dir.join(cartog_db::DB_FILE);
            }
            if !dir.pop() {
                break;
            }
        }
    }

    // 4. Fallback: DB_FILE relative to cwd
    PathBuf::from(cartog_db::DB_FILE)
}

/// Expand a leading `~/` to the user's home directory.
pub fn expand_tilde(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
            return PathBuf::from(home).join(rest);
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::fs;

    #[test]
    fn test_expand_tilde_with_home() {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| "/tmp".into());
        let expanded = expand_tilde(PathBuf::from("~/foo/bar"));
        assert_eq!(expanded, PathBuf::from(home).join("foo/bar"));
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
        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();

        let result = resolve_db_path(None, &CartogConfig::default());
        std::env::set_current_dir(original).unwrap();

        assert_eq!(result, PathBuf::from(cartog_db::DB_FILE));
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

        assert_eq!(result, canonical_root.join(cartog_db::DB_FILE));
    }

    // ── Embedding config tests ──

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
    fn test_config_unknown_fields_ignored() {
        let toml_str = r#"
[embedding]
provider = "local"
unknown_field = "should be ignored"
"#;
        // serde default: unknown fields are silently ignored
        let cfg: CartogConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.embedding.unwrap().provider(), "local");
    }

    // ── to_provider_config tests ──

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

    // ── RagConfig ──────────────────────────────────────────────────────

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
}
