use anyhow::{Context, Result};
use fastembed::TextRerank;

/// Cross-encoder re-ranker for scoring (query, document) pairs.
///
/// Uses ONNX Runtime via fastembed for inference. The configured reranker model
/// processes query and document jointly through all transformer layers,
/// producing a relevance score for each pair.
pub struct CrossEncoderEngine {
    model: TextRerank,
}

impl CrossEncoderEngine {
    /// Load the cross-encoder re-ranker for `model`, downloading the weights from
    /// HuggingFace on first use. Routes through
    /// `super::providers::local::load_text_rerank` so the fastembed variant is selected
    /// in exactly one place. Models are cached in the shared directory
    /// (see [`super::model_cache_dir`]).
    pub fn load(model: Option<&str>, intra_threads: Option<usize>) -> Result<Self> {
        Ok(Self {
            model: super::providers::local::load_text_rerank(model, intra_threads)?,
        })
    }

    /// Score multiple documents against a single query.
    ///
    /// Returns scores in the same order as the input documents.
    /// Uses index-based placement (O(n)) instead of sorting (O(n log n)).
    pub fn score_batch(&mut self, query: &str, documents: &[&str]) -> Result<Vec<f32>> {
        if documents.is_empty() {
            return Ok(Vec::new());
        }

        let results = self
            .model
            .rerank(query, documents, false, None)
            .context("Cross-encoder batch scoring failed")?;

        // Results come back sorted by score descending — place back by original index.
        let mut scores = vec![0.0f32; documents.len()];
        for r in &results {
            scores[r.index] = r.score;
        }

        Ok(scores)
    }
}
