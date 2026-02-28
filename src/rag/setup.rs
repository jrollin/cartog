use anyhow::{Context, Result};

use super::embeddings::EmbeddingEngine;
use super::model_cache_dir;
use super::reranker::CrossEncoderEngine;

/// Result of the setup operation.
#[derive(Debug, serde::Serialize)]
pub struct SetupResult {
    pub model_dir: String,
}

/// Download the embedding model by initializing the fastembed engine.
///
/// fastembed automatically downloads the ONNX model from HuggingFace on first use.
/// This function eagerly triggers that download so the user sees progress.
pub fn download_model() -> Result<SetupResult> {
    let cache_dir = model_cache_dir();

    let _engine =
        EmbeddingEngine::new_with_progress().context("Failed to download embedding model")?;

    Ok(SetupResult {
        model_dir: cache_dir.display().to_string(),
    })
}

/// Download the cross-encoder re-ranker model.
///
/// fastembed automatically downloads the ONNX model from HuggingFace on first use.
pub fn download_cross_encoder() -> Result<SetupResult> {
    let cache_dir = model_cache_dir();

    let _engine = CrossEncoderEngine::load_with_progress()
        .context("Failed to download cross-encoder model")?;

    Ok(SetupResult {
        model_dir: cache_dir.display().to_string(),
    })
}
