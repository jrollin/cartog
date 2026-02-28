use anyhow::{Context, Result};
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};

use super::{model_cache_dir, EMBEDDING_DIM};

/// Batch size for fastembed internal sub-batching.
/// Smaller batches reduce padding waste when text lengths vary widely.
const EMBED_BATCH_SIZE: usize = 64;

/// Embedding engine wrapping a fastembed ONNX model.
///
/// Uses ONNX Runtime for inference with SIMD and graph-level optimizations.
/// The quantized model (BGESmallENV15Q) is ~2-3x faster than full precision
/// with negligible quality loss.
pub struct EmbeddingEngine {
    model: TextEmbedding,
}

impl EmbeddingEngine {
    /// Create a new embedding engine using the quantized BGE-small-en-v1.5 model.
    ///
    /// Models are cached in the shared directory (see [`super::model_cache_dir`]).
    pub fn new() -> Result<Self> {
        let model = TextEmbedding::try_new(
            TextInitOptions::new(EmbeddingModel::BGESmallENV15Q)
                .with_cache_dir(model_cache_dir())
                .with_show_download_progress(false),
        )
        .context("Failed to initialize embedding model")?;

        Ok(Self { model })
    }

    /// Create a new embedding engine, showing download progress on stdout.
    pub fn new_with_progress() -> Result<Self> {
        let model = TextEmbedding::try_new(
            TextInitOptions::new(EmbeddingModel::BGESmallENV15Q)
                .with_cache_dir(model_cache_dir())
                .with_show_download_progress(true),
        )
        .context("Failed to initialize embedding model")?;

        Ok(Self { model })
    }

    /// Embed a single text string, returning a normalized vector.
    pub fn embed(&mut self, text: &str) -> Result<Vec<f32>> {
        let results = self
            .model
            .embed(vec![text], Some(1))
            .context("Embedding failed")?;

        let vec = results
            .into_iter()
            .next()
            .context("No embedding returned")?;

        debug_assert_eq!(
            vec.len(),
            EMBEDDING_DIM,
            "Expected {EMBEDDING_DIM}-dim embedding, got {}",
            vec.len()
        );

        Ok(vec)
    }

    /// Embed multiple texts in a batch.
    ///
    /// Accepts `&[&str]` to avoid forcing callers to own Strings.
    pub fn embed_batch(&mut self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let results = self
            .model
            .embed(texts, Some(EMBED_BATCH_SIZE))
            .context("Batch embedding failed")?;

        debug_assert!(
            results.iter().all(|v| v.len() == EMBEDDING_DIM),
            "All embeddings should be {EMBEDDING_DIM}-dim"
        );

        Ok(results)
    }
}

/// Serialize a Vec<f32> to little-endian bytes for sqlite-vec storage.
pub fn embedding_to_bytes(embedding: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(embedding.len() * 4);
    for &val in embedding {
        bytes.extend_from_slice(&val.to_le_bytes());
    }
    bytes
}

/// Deserialize little-endian bytes back to Vec<f32>.
#[allow(dead_code)]
pub fn bytes_to_embedding(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_embedding_roundtrip() {
        let original = vec![0.1_f32, -0.5, 1.0, 0.0, std::f32::consts::PI];
        let bytes = embedding_to_bytes(&original);
        let restored = bytes_to_embedding(&bytes);
        assert_eq!(original, restored);
    }

    #[test]
    fn test_embedding_byte_length() {
        let vec = vec![0.0_f32; EMBEDDING_DIM];
        let bytes = embedding_to_bytes(&vec);
        assert_eq!(bytes.len(), EMBEDDING_DIM * 4);
    }

    #[test]
    fn test_empty_bytes_roundtrip() {
        let original: Vec<f32> = vec![];
        let bytes = embedding_to_bytes(&original);
        assert!(bytes.is_empty());
        let restored = bytes_to_embedding(&bytes);
        assert!(restored.is_empty());
    }

    /// Engine-level test: verifies the model produces correct-dimension embeddings.
    /// Requires the embedding model to be downloaded (skipped in CI if unavailable).
    #[test]
    fn test_engine_embed_dimension() {
        let mut engine = match EmbeddingEngine::new() {
            Ok(e) => e,
            Err(_) => return, // model not available, skip
        };

        let vec = engine.embed("fn validate_token(token: &str)").unwrap();
        assert_eq!(vec.len(), EMBEDDING_DIM);

        // Verify embedding is normalized (L2 norm ≈ 1.0)
        let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < 0.01,
            "embedding should be L2-normalized, got norm={norm}"
        );
    }

    /// Engine-level test: batch embedding produces correct dimensions and count.
    #[test]
    fn test_engine_embed_batch() {
        let mut engine = match EmbeddingEngine::new() {
            Ok(e) => e,
            Err(_) => return, // model not available, skip
        };

        let texts = [
            "fn foo() -> i32",
            "class AuthService",
            "def validate(token)",
        ];
        let results = engine.embed_batch(&texts).unwrap();
        assert_eq!(results.len(), 3);
        for v in &results {
            assert_eq!(v.len(), EMBEDDING_DIM);
        }
    }

    #[test]
    fn test_engine_embed_batch_empty() {
        let mut engine = match EmbeddingEngine::new() {
            Ok(e) => e,
            Err(_) => return,
        };

        let texts: &[&str] = &[];
        let results = engine.embed_batch(texts).unwrap();
        assert!(results.is_empty());
    }
}
