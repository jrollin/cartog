//! Semantic search and RAG pipeline for cartog.
//!
//! Combines FTS5 keyword search (BM25) with vector KNN search using Reciprocal
//! Rank Fusion, then optionally reranks with a cross-encoder.
//!
//! Supports pluggable embedding providers via the [`provider::EmbeddingProvider`] trait:
//! - **local** (default): ONNX models via fastembed (feature `provider-local`)
//! - **ollama**: HTTP API to an Ollama server (feature `provider-ollama`)
//! - **openai**: OpenAI-compatible `/v1/embeddings` endpoints (feature `provider-openai`)
#![cfg_attr(docsrs, feature(doc_cfg))]
#![doc = ""]
#![doc = include_str!("../README.md")]

pub mod context;
#[cfg(feature = "provider-local")]
pub mod embeddings;
pub mod indexer;
pub mod provider;
pub mod providers;
#[cfg(feature = "provider-local")]
pub mod reranker;
pub mod search;
#[cfg(feature = "provider-local")]
pub mod setup;

/// Default reranker model (HuggingFace repo path). Small, fast, higher BEIR NDCG@10
/// than the former bge-reranker-base default. Resolved when `[reranker] model` is unset.
pub const DEFAULT_RERANKER_MODEL: &str = "jinaai/jina-reranker-v1-turbo-en";

/// Default embedding dimension (re-exported from cartog-db for convenience).
pub const EMBEDDING_DIM: usize = cartog_db::DEFAULT_EMBEDDING_DIM;

/// Snapshot the identity of a live `EmbeddingProvider` for the on-disk
/// fingerprint check. Used by callers right after `Database::open` to
/// detect (and clear) a stale `symbol_vec` when the user swaps provider
/// or model.
pub fn fingerprint_of(p: &dyn provider::EmbeddingProvider) -> cartog_db::EmbeddingFingerprint {
    cartog_db::EmbeddingFingerprint {
        provider: p.name().to_string(),
        model: p.model_id().to_string(),
        dimension: p.dimension(),
    }
}

/// Parameters for creating an embedding provider.
#[derive(Clone)]
pub struct EmbeddingProviderConfig {
    /// Provider type: "local", "ollama", or "openai".
    pub provider: String,
    /// Model name (provider-specific). None = provider default.
    pub model: Option<String>,
    /// Explicit dimension override. None = auto-detect from model/provider.
    pub dimension: Option<usize>,
    /// Query prefix for asymmetric models.
    pub query_prefix: Option<String>,
    /// Document prefix for asymmetric models.
    pub document_prefix: Option<String>,
    /// Base URL for remote providers (Ollama, OpenAI). None = provider default.
    pub base_url: Option<String>,
    /// Env var name holding the OpenAI API key. None = provider default (`OPENAI_API_KEY`).
    pub api_key_env: Option<String>,
    /// Reranker provider: "local" (default) or "none".
    pub reranker_provider: String,
    /// Reranker model as a fastembed HF repo path (e.g. `BAAI/bge-reranker-base`).
    /// None = [`DEFAULT_RERANKER_MODEL`]. Mirrors `model` for embeddings.
    pub reranker_model: Option<String>,
    /// Optional cap on ONNX intra-op threads for the local provider. None =
    /// fastembed's default (all cores). `CARTOG_ONNX_THREADS` overrides this.
    pub intra_threads: Option<usize>,
}

impl Default for EmbeddingProviderConfig {
    fn default() -> Self {
        Self {
            provider: "local".to_string(),
            model: None,
            dimension: None,
            query_prefix: None,
            document_prefix: None,
            base_url: None,
            api_key_env: None,
            reranker_provider: "local".to_string(),
            reranker_model: None,
            intra_threads: None,
        }
    }
}

impl EmbeddingProviderConfig {
    /// Resolve the embedding dimension for this config.
    /// Uses explicit dimension if set, otherwise falls back to the local provider default (384).
    /// For Ollama, the actual dimension is auto-detected at provider construction time;
    /// this method should not be relied upon for non-local providers without an explicit dimension.
    pub fn resolved_dimension(&self) -> usize {
        self.dimension.unwrap_or(EMBEDDING_DIM)
    }
}

/// Create an embedding provider from the given configuration.
pub fn create_embedding_provider(
    config: &EmbeddingProviderConfig,
) -> anyhow::Result<Box<dyn provider::EmbeddingProvider>> {
    match config.provider.as_str() {
        #[cfg(feature = "provider-local")]
        "local" => {
            let provider = providers::local::LocalEmbeddingProvider::new(
                config.model.as_deref(),
                config.query_prefix.clone(),
                config.document_prefix.clone(),
                config.intra_threads,
            )?;
            Ok(Box::new(provider))
        }
        #[cfg(feature = "provider-ollama")]
        "ollama" => {
            let provider = providers::ollama::OllamaEmbeddingProvider::new(
                config.base_url.as_deref(),
                config.model.as_deref(),
                config.dimension,
            )?;
            Ok(Box::new(provider))
        }
        #[cfg(feature = "provider-openai")]
        "openai" => {
            let provider = providers::openai::OpenAiEmbeddingProvider::new(
                config.base_url.as_deref(),
                config.model.as_deref(),
                config.dimension,
                config.api_key_env.as_deref(),
            )?;
            Ok(Box::new(provider))
        }
        other => {
            // `ollama`/`openai` ship in the default build; they are only absent
            // from a feature-stripped `--no-default-features` rebuild. Point such
            // a user at restoring the feature instead of a bare "unknown".
            let remediation = match other {
                "ollama" if !cfg!(feature = "provider-ollama") => {
                    " — this build was compiled with `--no-default-features`; rebuild with \
                     default features or add `--features ollama-embedding`"
                }
                "openai" if !cfg!(feature = "provider-openai") => {
                    " — this build was compiled with `--no-default-features`; rebuild with \
                     default features or add `--features openai-embedding`"
                }
                _ => "",
            };
            anyhow::bail!(
                "Unknown or disabled embedding provider: '{other}'. Supported: {}{remediation}",
                supported_providers()
            )
        }
    }
}

fn supported_providers() -> String {
    let mut names = Vec::new();
    if cfg!(feature = "provider-local") {
        names.push("local");
    }
    if cfg!(feature = "provider-ollama") {
        names.push("ollama");
    }
    if cfg!(feature = "provider-openai") {
        names.push("openai");
    }
    if names.is_empty() {
        return "none (enable provider features)".to_string();
    }
    names.join(", ")
}

/// Create the default local embedding provider (BGE-small-en-v1.5 quantized).
pub fn create_default_embedding_provider() -> anyhow::Result<Box<dyn provider::EmbeddingProvider>> {
    create_embedding_provider(&EmbeddingProviderConfig::default())
}

/// Create a reranker provider based on the given provider name.
///
/// - `"local"` — loads the local ONNX cross-encoder (requires `provider-local` feature)
/// - `"none"` — disables re-ranking
///
/// `model` is the fastembed reranker HF repo path; `None` = [`DEFAULT_RERANKER_MODEL`].
/// Ignored for `"none"`.
///
/// Returns `None` if re-ranking is disabled, the model is unavailable, or the feature is off.
pub fn create_reranker_provider(
    reranker_provider: &str,
    model: Option<&str>,
    intra_threads: Option<usize>,
) -> Option<Box<dyn provider::RerankerProvider>> {
    // `model`/`intra_threads` are only consumed by the local provider; without that
    // feature they would trip `-D unused-variables`.
    #[cfg(not(feature = "provider-local"))]
    let _ = (model, intra_threads);
    match reranker_provider {
        "none" => None,
        #[cfg(feature = "provider-local")]
        "local" => match providers::local::LocalRerankerProvider::load(model, intra_threads) {
            Ok(r) => Some(Box::new(r)),
            Err(e) => {
                tracing::warn!(error = %e, "Cross-encoder not available, skipping re-ranking");
                None
            }
        },
        other => {
            tracing::warn!(
                provider = other,
                "Unknown reranker provider, skipping re-ranking"
            );
            None
        }
    }
}

/// Create the default local reranker provider ([`DEFAULT_RERANKER_MODEL`]).
pub fn create_default_reranker_provider() -> Option<Box<dyn provider::RerankerProvider>> {
    create_reranker_provider("local", None, None)
}

// ── Local ONNX model cache management (provider-local only) ──

/// Check if a model is already downloaded in the hf-hub cache (no network access).
///
/// Mirrors `hf_hub::CacheRepo::get()` logic: reads the commit hash from
/// `<cache>/models--<org>--<name>/refs/main`, then checks for the ONNX file
/// in `snapshots/<hash>/<model_file>`.
#[cfg(feature = "provider-local")]
fn is_model_cached(model_code: &str, model_file: &str) -> bool {
    let cache_dir = model_cache_dir();
    let dir_name = format!("models--{}", model_code.replace('/', "--"));
    let ref_path = cache_dir.join(&dir_name).join("refs").join("main");
    let Ok(commit_hash) = std::fs::read_to_string(&ref_path) else {
        return false;
    };
    let model_path = cache_dir
        .join(&dir_name)
        .join("snapshots")
        .join(commit_hash.trim())
        .join(model_file);
    model_path.exists()
}

/// Check whether every file of an `(model_code, model_file, additional_files)` triple
/// is present in the hf-hub cache. The `additional_files` cover models with a separate
/// weights sidecar (e.g. `model.onnx.data`).
#[cfg(feature = "provider-local")]
fn are_model_files_cached(model_code: &str, model_file: &str, additional_files: &[String]) -> bool {
    is_model_cached(model_code, model_file)
        && additional_files
            .iter()
            .all(|f| is_model_cached(model_code, f))
}

/// Resolve an embedding `model` name (None = the default BGE-small) to its fastembed
/// variant. Errors on an unknown name, mirroring [`resolve_reranker_model`].
#[cfg(feature = "provider-local")]
pub(crate) fn resolve_embedding_model(
    model: Option<&str>,
) -> anyhow::Result<fastembed::EmbeddingModel> {
    match model {
        Some(name) => name
            .parse::<fastembed::EmbeddingModel>()
            .map_err(|e| anyhow::anyhow!("{e}")),
        None => Ok(fastembed::EmbeddingModel::BGESmallENV15Q),
    }
}

/// Check if `model`'s embedding weights are already in the hf-hub cache (no network).
///
/// Derives the cache identity from fastembed so a configured non-default `[embedding]
/// model` is checked against its own cache dir, not the default's. An unknown/unparseable
/// `model` is treated as "not cached" (the loader surfaces the error).
#[cfg(feature = "provider-local")]
pub fn is_embedding_model_cached(model: Option<&str>) -> bool {
    let Ok(em) = resolve_embedding_model(model) else {
        return false;
    };
    let Ok(info) = fastembed::TextEmbedding::get_model_info(&em) else {
        return false;
    };
    are_model_files_cached(&info.model_code, &info.model_file, &info.additional_files)
}

/// Resolve a reranker `model` repo path (None = [`DEFAULT_RERANKER_MODEL`]) to its
/// fastembed variant. Errors on an unknown repo path. Mirrors embedding's model parse.
#[cfg(feature = "provider-local")]
pub fn resolve_reranker_model(model: Option<&str>) -> anyhow::Result<fastembed::RerankerModel> {
    model
        .unwrap_or(DEFAULT_RERANKER_MODEL)
        .parse::<fastembed::RerankerModel>()
        .map_err(|e| anyhow::anyhow!("{e}"))
}

/// Check if a already-resolved reranker model's weights are in the hf-hub cache.
/// Takes the resolved variant so the caller (which already parsed it) avoids re-parsing.
#[cfg(feature = "provider-local")]
pub fn is_reranker_resolved_cached(rm: &fastembed::RerankerModel) -> bool {
    let info = fastembed::TextRerank::get_model_info(rm);
    are_model_files_cached(&info.model_code, &info.model_file, &info.additional_files)
}

/// hf-hub cache directory for the former default reranker (`bge-reranker-base`).
///
/// Derives the `models--<org>--<name>` dir from fastembed's model code (the same
/// transform `is_model_cached` uses) rather than hardcoding the literal, so it tracks
/// any fastembed repo-path change. Used by `cartog doctor` to offer a reclaim hint when
/// the now-orphaned ~1.1GB model lingers under the new default.
#[cfg(feature = "provider-local")]
pub fn legacy_bge_reranker_cache_dir() -> std::path::PathBuf {
    let code = fastembed::TextRerank::get_model_info(&fastembed::RerankerModel::BGERerankerBase)
        .model_code;
    model_cache_dir().join(format!("models--{}", code.replace('/', "--")))
}

/// Check if `model`'s reranker weights are already in the hf-hub cache (no network).
///
/// Convenience wrapper over [`is_reranker_resolved_cached`] for callers holding a config
/// string. An unknown/unparseable `model` is treated as "not cached"; callers that need to
/// distinguish "invalid" from "uncached" should call [`resolve_reranker_model`] first.
#[cfg(feature = "provider-local")]
pub fn is_reranker_model_cached(model: Option<&str>) -> bool {
    match resolve_reranker_model(model) {
        Ok(rm) => is_reranker_resolved_cached(&rm),
        Err(_) => false,
    }
}

/// Shared model cache directory for ONNX models (embedding + reranker).
///
/// Precedence:
/// 1. `FASTEMBED_CACHE_DIR` env var (fastembed's own convention)
/// 2. `XDG_CACHE_HOME/cartog/models` (XDG standard)
/// 3. `~/.cache/cartog/models` (fallback)
///
/// This avoids downloading 1.2GB of models per project (fastembed's default is
/// `.fastembed_cache` in CWD).
pub fn model_cache_dir() -> std::path::PathBuf {
    // 1. Respect fastembed's own env var
    if let Ok(dir) = std::env::var("FASTEMBED_CACHE_DIR") {
        return std::path::PathBuf::from(dir);
    }

    // 2. XDG_CACHE_HOME / cartog / models
    if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
        return std::path::PathBuf::from(xdg).join("cartog").join("models");
    }

    // 3. ~/.cache/cartog/models
    if let Some(home) = home_dir() {
        return home.join(".cache").join("cartog").join("models");
    }

    // Last resort: fastembed's default (CWD/.fastembed_cache)
    std::path::PathBuf::from(".fastembed_cache")
}

/// Get the user's home directory (no external dependency needed).
fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE")) // Windows fallback
        .ok()
        .map(std::path::PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_cache_dir_is_not_local() {
        // Unless FASTEMBED_CACHE_DIR is explicitly set to a local path,
        // model_cache_dir should NOT return ".fastembed_cache" (the per-project default).
        let dir = model_cache_dir();
        let dir_str = dir.to_string_lossy();
        // On any system with HOME set, this should be an absolute path
        if std::env::var("FASTEMBED_CACHE_DIR").is_err() {
            assert!(
                dir_str.contains("cartog"),
                "cache dir should contain 'cartog', got: {dir_str}"
            );
            assert!(
                !dir_str.starts_with('.'),
                "cache dir should be absolute, not relative: {dir_str}"
            );
        }
    }

    #[test]
    fn test_model_cache_dir_ends_with_models() {
        if std::env::var("FASTEMBED_CACHE_DIR").is_err() {
            let dir = model_cache_dir();
            assert!(
                dir.ends_with("models"),
                "cache dir should end with 'models', got: {}",
                dir.display()
            );
        }
    }

    #[test]
    fn test_boxed_reranker_as_deref_mut_some() {
        use provider::test_utils::MockRerankerProvider;

        let mut reranker: Option<Box<dyn provider::RerankerProvider>> =
            Some(Box::new(MockRerankerProvider));
        let r = reranker.as_deref_mut();
        assert!(r.is_some());
        assert_eq!(r.unwrap().name(), "mock-reranker");
    }

    #[test]
    fn test_boxed_reranker_as_deref_mut_none() {
        let mut reranker: Option<Box<dyn provider::RerankerProvider>> = None;
        let r = reranker.as_deref_mut();
        assert!(r.is_none());
    }

    #[test]
    fn test_embedding_dim_constant() {
        assert_eq!(EMBEDDING_DIM, 384);
        assert_eq!(EMBEDDING_DIM, cartog_db::DEFAULT_EMBEDDING_DIM);
    }

    #[test]
    fn test_create_embedding_provider_invalid_provider() {
        let config = EmbeddingProviderConfig {
            provider: "nonexistent".to_string(),
            ..Default::default()
        };
        let result = create_embedding_provider(&config);
        let err = result.err().expect("should be an error").to_string();
        assert!(
            err.contains("nonexistent"),
            "error should mention the invalid provider name: {err}"
        );
    }

    #[test]
    fn test_provider_config_default_values() {
        let config = EmbeddingProviderConfig::default();
        assert_eq!(config.provider, "local");
        assert!(config.model.is_none());
        assert!(config.dimension.is_none());
        assert!(config.query_prefix.is_none());
        assert!(config.document_prefix.is_none());
        assert!(config.base_url.is_none());
        assert!(config.api_key_env.is_none());
        assert_eq!(config.reranker_provider, "local");
        assert!(config.reranker_model.is_none());
        assert_eq!(config.resolved_dimension(), 384);
    }

    #[cfg(feature = "provider-local")]
    #[test]
    fn test_resolve_reranker_model_default_and_known_and_unknown() {
        // None resolves to the default; a known repo path parses; an unknown errors.
        assert_eq!(
            resolve_reranker_model(None).unwrap(),
            fastembed::RerankerModel::JINARerankerV1TurboEn
        );
        assert_eq!(
            resolve_reranker_model(Some("BAAI/bge-reranker-base")).unwrap(),
            fastembed::RerankerModel::BGERerankerBase
        );
        let err = resolve_reranker_model(Some("nope/not-real")).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("unknown"));
    }

    #[cfg(feature = "provider-local")]
    #[test]
    fn test_default_reranker_model_const_resolves() {
        // DEFAULT_RERANKER_MODEL must be a real fastembed repo path.
        assert!(DEFAULT_RERANKER_MODEL
            .parse::<fastembed::RerankerModel>()
            .is_ok());
    }

    #[cfg(feature = "provider-local")]
    #[test]
    fn test_resolve_embedding_model_default_and_unknown() {
        assert_eq!(
            resolve_embedding_model(None).unwrap(),
            fastembed::EmbeddingModel::BGESmallENV15Q
        );
        assert!(resolve_embedding_model(Some("not-a-real-embedding-model")).is_err());
    }

    #[test]
    fn test_provider_config_resolved_dimension_with_explicit() {
        let config = EmbeddingProviderConfig {
            dimension: Some(768),
            ..Default::default()
        };
        assert_eq!(config.resolved_dimension(), 768);
    }

    #[test]
    fn test_supported_providers_includes_local() {
        let s = supported_providers();
        assert!(s.contains("local"), "should include local: {s}");
    }
}
