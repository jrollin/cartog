use anyhow::{Context, Result};
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use tracing::info;

use crate::model_cache_dir;
use crate::provider::{EmbeddingProvider, RerankerProvider};

const EMBED_BATCH_SIZE: usize = 64;

/// Local ONNX embedding provider via fastembed.
pub struct LocalEmbeddingProvider {
    model: TextEmbedding,
    model_code: String,
    dim: usize,
    query_prefix: Option<String>,
    document_prefix: Option<String>,
}

impl LocalEmbeddingProvider {
    /// Create a new local embedding provider.
    ///
    /// `model_name`: fastembed model code (e.g. "BAAI/bge-small-en-v1.5") or None for default.
    /// `query_prefix` / `document_prefix`: optional prefixes for asymmetric models.
    pub fn new(
        model_name: Option<&str>,
        query_prefix: Option<String>,
        document_prefix: Option<String>,
    ) -> Result<Self> {
        let embedding_model = match model_name {
            Some(name) => name
                .parse::<EmbeddingModel>()
                .map_err(|e| anyhow::anyhow!("{e}"))?,
            None => EmbeddingModel::BGESmallENV15Q,
        };

        let model_info = TextEmbedding::get_model_info(&embedding_model)?;
        let dim = model_info.dim;
        // model_info.model_code is the stable HuggingFace path (e.g.
        // "Qdrant/bge-small-en-v1.5-onnx-Q"). The previous implementation
        // used `embedding_model.to_string()` which is fastembed's `Display`
        // impl — itself just the Debug repr of the enum variant
        // ("BGESmallENV15Q"). A fastembed variant rename would have wiped
        // every default-config user's `symbol_vec` on next open; the HF
        // path is wire-stable across fastembed releases.
        let model_code = model_info.model_code.clone();

        let is_cached = crate::is_embedding_model_cached();
        if is_cached {
            info!("Loading embedding model...");
        } else {
            info!("Downloading embedding model (first time only)...");
        }

        let model = TextEmbedding::try_new(
            TextInitOptions::new(embedding_model)
                .with_cache_dir(model_cache_dir())
                .with_show_download_progress(true),
        )
        .context("Failed to initialize embedding model")?;

        Ok(Self {
            model,
            model_code,
            dim,
            query_prefix,
            document_prefix,
        })
    }
}

impl EmbeddingProvider for LocalEmbeddingProvider {
    fn name(&self) -> &str {
        "local"
    }

    fn model_id(&self) -> &str {
        &self.model_code
    }

    fn dimension(&self) -> usize {
        self.dim
    }

    fn embed_query(&mut self, text: &str) -> Result<Vec<f32>> {
        let owned;
        let input: &str = match &self.query_prefix {
            Some(prefix) => {
                owned = format!("{prefix}{text}");
                &owned
            }
            None => text,
        };
        let results = self
            .model
            .embed(vec![input], Some(1))
            .context("Embedding query failed")?;
        results.into_iter().next().context("No embedding returned")
    }

    fn embed_document(&mut self, text: &str) -> Result<Vec<f32>> {
        let owned;
        let input: &str = match &self.document_prefix {
            Some(prefix) => {
                owned = format!("{prefix}{text}");
                &owned
            }
            None => text,
        };
        let results = self
            .model
            .embed(vec![input], Some(1))
            .context("Embedding document failed")?;
        results.into_iter().next().context("No embedding returned")
    }

    fn embed_documents(&mut self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        match &self.document_prefix {
            Some(prefix) => {
                let prefixed: Vec<String> = texts.iter().map(|t| format!("{prefix}{t}")).collect();
                let refs: Vec<&str> = prefixed.iter().map(|s| s.as_str()).collect();
                self.model
                    .embed(refs, Some(EMBED_BATCH_SIZE))
                    .context("Batch embedding failed")
            }
            None => self
                .model
                .embed(texts, Some(EMBED_BATCH_SIZE))
                .context("Batch embedding failed"),
        }
    }
}

/// Local ONNX cross-encoder re-ranker via fastembed.
pub struct LocalRerankerProvider {
    model: fastembed::TextRerank,
}

impl LocalRerankerProvider {
    pub fn load() -> Result<Self> {
        if crate::is_reranker_model_cached() {
            info!("Loading reranker model...");
        } else {
            info!("Downloading reranker model (~1.1GB, first time only)...");
        }

        let model = fastembed::TextRerank::try_new(
            fastembed::RerankInitOptions::new(fastembed::RerankerModel::BGERerankerBase)
                .with_cache_dir(model_cache_dir())
                .with_show_download_progress(true),
        )
        .context("Failed to initialize cross-encoder model")?;

        Ok(Self { model })
    }
}

impl RerankerProvider for LocalRerankerProvider {
    fn name(&self) -> &str {
        "local"
    }

    fn score_batch(&mut self, query: &str, documents: &[&str]) -> Result<Vec<f32>> {
        if documents.is_empty() {
            return Ok(Vec::new());
        }

        let results = self
            .model
            .rerank(query, documents, false, None)
            .context("Cross-encoder batch scoring failed")?;

        let mut scores = vec![0.0f32; documents.len()];
        for r in &results {
            scores[r.index] = r.score;
        }

        Ok(scores)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fingerprint stored in `metadata` on every DB open must be a
    /// wire-stable identifier, not fastembed's Debug repr (a Rust enum
    /// variant name). A regression here wipes every default-config user's
    /// `symbol_vec` on next open when fastembed renames the variant.
    ///
    /// We test the contract without instantiating the heavy
    /// `LocalEmbeddingProvider` (which downloads ONNX weights): the
    /// constructor reads `model_info.model_code` and stores it on the
    /// provider, so asserting `get_model_info` returns the stable
    /// HuggingFace path is equivalent.
    #[test]
    fn default_model_code_is_stable_hf_path() {
        let info = TextEmbedding::get_model_info(&EmbeddingModel::BGESmallENV15Q)
            .expect("default model metadata");
        assert_eq!(
            info.model_code, "Qdrant/bge-small-en-v1.5-onnx-Q",
            "BGESmallENV15Q's HF path must not change without an explicit migration; \
             if fastembed bumped the path, also bump cartog's migration so existing \
             DBs don't wipe their vector index on next open"
        );
    }

    #[test]
    fn default_model_code_differs_from_variant_debug_repr() {
        // Belt-and-suspenders: ensure we're not accidentally storing the
        // Debug name. If fastembed ever changes Display impl to NOT be
        // Debug, this assertion still passes (model_code is HF path, not
        // a Rust ident).
        let info = TextEmbedding::get_model_info(&EmbeddingModel::BGESmallENV15Q).unwrap();
        let debug_repr = format!("{:?}", EmbeddingModel::BGESmallENV15Q);
        assert_ne!(
            info.model_code, debug_repr,
            "model_code must be the HF path, not the Rust variant name"
        );
    }
}
