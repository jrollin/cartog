//! Semantic search and RAG pipeline for cartog.
//!
//! Combines FTS5 keyword search (BM25) with vector KNN search using Reciprocal
//! Rank Fusion, then optionally reranks with a cross-encoder.
//!
//! Supports pluggable embedding providers via the [`provider::EmbeddingProvider`] trait:
//! - **local** (default): ONNX models via fastembed (feature `provider-local`)
//! - **ollama**: HTTP API to an Ollama server (feature `provider-ollama`)

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
    /// Provider type: "local" or "ollama".
    pub provider: String,
    /// Model name (provider-specific). None = provider default.
    pub model: Option<String>,
    /// Explicit dimension override. None = auto-detect from model/provider.
    pub dimension: Option<usize>,
    /// Query prefix for asymmetric models.
    pub query_prefix: Option<String>,
    /// Document prefix for asymmetric models.
    pub document_prefix: Option<String>,
    /// Base URL for remote providers (Ollama). None = provider default.
    pub base_url: Option<String>,
    /// Reranker provider: "local" (default) or "none".
    pub reranker_provider: String,
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
            reranker_provider: "local".to_string(),
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
        other => {
            // `ollama` ships in the default build; it is only absent from a
            // feature-stripped `--no-default-features` rebuild. Point such a
            // user at restoring the feature instead of a bare "unknown".
            let remediation = if other == "ollama" && !cfg!(feature = "provider-ollama") {
                " — this build was compiled with `--no-default-features`; rebuild with \
                 default features or add `--features ollama-embedding`"
            } else {
                ""
            };
            anyhow::bail!(
                "Unknown or disabled embedding provider: '{other}'. Supported: {}{remediation}",
                supported_providers()
            )
        }
    }
}

fn supported_providers() -> &'static str {
    match (
        cfg!(feature = "provider-local"),
        cfg!(feature = "provider-ollama"),
    ) {
        (true, true) => "local, ollama",
        (true, false) => "local",
        (false, true) => "ollama",
        (false, false) => "none (enable provider features)",
    }
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
/// Returns `None` if re-ranking is disabled, the model is unavailable, or the feature is off.
pub fn create_reranker_provider(
    reranker_provider: &str,
) -> Option<Box<dyn provider::RerankerProvider>> {
    match reranker_provider {
        "none" => None,
        #[cfg(feature = "provider-local")]
        "local" => match providers::local::LocalRerankerProvider::load() {
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

/// Create the default local reranker provider (BGE-reranker-base).
pub fn create_default_reranker_provider() -> Option<Box<dyn provider::RerankerProvider>> {
    create_reranker_provider("local")
}

// ── Local ONNX model cache management (provider-local only) ──

#[cfg(feature = "provider-local")]
const EMBEDDING_MODEL_CODE: &str = "Qdrant/bge-small-en-v1.5-onnx-Q";
#[cfg(feature = "provider-local")]
const EMBEDDING_MODEL_FILE: &str = "model_optimized.onnx";
#[cfg(feature = "provider-local")]
const RERANKER_MODEL_CODE: &str = "BAAI/bge-reranker-base";
#[cfg(feature = "provider-local")]
const RERANKER_MODEL_FILE: &str = "onnx/model.onnx";

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

#[cfg(feature = "provider-local")]
pub fn is_embedding_model_cached() -> bool {
    is_model_cached(EMBEDDING_MODEL_CODE, EMBEDDING_MODEL_FILE)
}

#[cfg(feature = "provider-local")]
pub fn is_reranker_model_cached() -> bool {
    is_model_cached(RERANKER_MODEL_CODE, RERANKER_MODEL_FILE)
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
        assert_eq!(config.resolved_dimension(), 384);
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
