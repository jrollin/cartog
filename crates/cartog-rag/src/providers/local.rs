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

        let is_cached = crate::is_embedding_model_cached();
        if is_cached {
            info!("Loading embedding model...");
        } else {
            info!("Downloading embedding model (first time only)...");
        }

        let model_code = embedding_model.to_string();

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
