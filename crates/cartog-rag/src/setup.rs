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
/// Progress and download status are logged via tracing (visible in non-TTY environments).
pub fn download_model() -> Result<SetupResult> {
    let cache_dir = model_cache_dir();

    let _engine = EmbeddingEngine::new().context("Failed to download embedding model")?;

    Ok(SetupResult {
        model_dir: cache_dir.display().to_string(),
    })
}

/// Download the cross-encoder re-ranker `model`.
///
/// fastembed automatically downloads the ONNX model from HuggingFace on first use.
/// Progress and download status are logged via tracing (visible in non-TTY environments).
pub fn download_cross_encoder(
    model: Option<&str>,
    intra_threads: Option<usize>,
) -> Result<SetupResult> {
    let cache_dir = model_cache_dir();

    let _engine = CrossEncoderEngine::load(model, intra_threads)
        .context("Failed to download cross-encoder model")?;

    Ok(SetupResult {
        model_dir: cache_dir.display().to_string(),
    })
}
