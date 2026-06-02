use anyhow::Result;

/// Serialize a `Vec<f32>` to little-endian bytes for sqlite-vec storage.
pub fn embedding_to_bytes(embedding: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(embedding.len() * 4);
    for &val in embedding {
        bytes.extend_from_slice(&val.to_le_bytes());
    }
    bytes
}

/// Deserialize little-endian bytes back to `Vec<f32>`.
#[allow(dead_code)]
pub fn bytes_to_embedding(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

/// Trait for embedding providers (local ONNX, Ollama, OpenAI, etc.).
///
/// Implementations handle model-specific details like query/document prefixes
/// (e.g. nomic prepends `"search_query:"` vs `"search_document:"`).
pub trait EmbeddingProvider: Send {
    /// Short identifier of the provider class (`"local"`, `"ollama"`, …).
    fn name(&self) -> &str;
    /// Identifier of the specific model in use. Combined with [`Self::name`]
    /// and [`Self::dimension`], forms the fingerprint stored in `metadata`
    /// to detect when the user swaps embedding stacks and the existing
    /// vector index becomes meaningless.
    fn model_id(&self) -> &str;
    fn dimension(&self) -> usize;

    /// Embed a query (for search). Some models use a different prefix than documents.
    fn embed_query(&mut self, text: &str) -> Result<Vec<f32>> {
        self.embed_document(text)
    }

    /// Embed a document (for indexing).
    fn embed_document(&mut self, text: &str) -> Result<Vec<f32>>;

    /// Embed multiple documents in a batch.
    fn embed_documents(&mut self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;
}

/// Trait for cross-encoder re-ranking providers.
pub trait RerankerProvider: Send {
    fn name(&self) -> &str;

    /// Score multiple documents against a single query.
    /// Returns scores in the same order as the input documents.
    fn score_batch(&mut self, query: &str, documents: &[&str]) -> Result<Vec<f32>>;
}

// `#[doc(hidden)]`: exposed under `test-util` for dependent crates' tests, but
// not a stable public API — these mocks carry no semver guarantee.
#[cfg(any(test, feature = "test-util"))]
#[doc(hidden)]
pub mod test_utils {
    use super::*;

    /// Mock embedding provider that returns deterministic vectors for testing.
    /// The vector is seeded from the text hash so identical inputs get identical outputs.
    pub struct MockEmbeddingProvider {
        pub dim: usize,
        pub embed_count: usize,
        pub query_prefix: Option<String>,
        pub document_prefix: Option<String>,
    }

    impl MockEmbeddingProvider {
        pub fn new(dim: usize) -> Self {
            Self {
                dim,
                embed_count: 0,
                query_prefix: None,
                document_prefix: None,
            }
        }

        pub fn with_prefixes(dim: usize, query: &str, document: &str) -> Self {
            Self {
                dim,
                embed_count: 0,
                query_prefix: Some(query.to_string()),
                document_prefix: Some(document.to_string()),
            }
        }

        fn deterministic_vector(&self, text: &str) -> Vec<f32> {
            let hash = text
                .bytes()
                .fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64));
            (0..self.dim)
                .map(|i| ((hash.wrapping_add(i as u64) % 1000) as f32) / 1000.0)
                .collect()
        }
    }

    impl EmbeddingProvider for MockEmbeddingProvider {
        fn name(&self) -> &str {
            "mock"
        }

        fn model_id(&self) -> &str {
            "mock-model"
        }

        fn dimension(&self) -> usize {
            self.dim
        }

        fn embed_query(&mut self, text: &str) -> Result<Vec<f32>> {
            self.embed_count += 1;
            let input = match &self.query_prefix {
                Some(p) => format!("{p}{text}"),
                None => text.to_string(),
            };
            Ok(self.deterministic_vector(&input))
        }

        fn embed_document(&mut self, text: &str) -> Result<Vec<f32>> {
            self.embed_count += 1;
            let input = match &self.document_prefix {
                Some(p) => format!("{p}{text}"),
                None => text.to_string(),
            };
            Ok(self.deterministic_vector(&input))
        }

        fn embed_documents(&mut self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
            texts.iter().map(|t| self.embed_document(t)).collect()
        }
    }

    /// Mock reranker that returns scores based on keyword overlap.
    pub struct MockRerankerProvider;

    impl RerankerProvider for MockRerankerProvider {
        fn name(&self) -> &str {
            "mock-reranker"
        }

        fn score_batch(&mut self, query: &str, documents: &[&str]) -> Result<Vec<f32>> {
            let query_words: std::collections::HashSet<&str> = query.split_whitespace().collect();
            Ok(documents
                .iter()
                .map(|doc| {
                    let doc_words: std::collections::HashSet<&str> =
                        doc.split_whitespace().collect();
                    query_words.intersection(&doc_words).count() as f32
                })
                .collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_utils::*;
    use super::*;

    #[test]
    fn mock_provider_returns_correct_dimension() {
        let mut provider = MockEmbeddingProvider::new(768);
        assert_eq!(provider.dimension(), 768);
        assert_eq!(provider.name(), "mock");

        let vec = provider.embed_document("test").unwrap();
        assert_eq!(vec.len(), 768);
    }

    #[test]
    fn mock_provider_deterministic_output() {
        let mut p1 = MockEmbeddingProvider::new(384);
        let mut p2 = MockEmbeddingProvider::new(384);

        let v1 = p1.embed_document("hello world").unwrap();
        let v2 = p2.embed_document("hello world").unwrap();
        assert_eq!(v1, v2);

        let v3 = p1.embed_document("different text").unwrap();
        assert_ne!(v1, v3);
    }

    #[test]
    fn embed_query_defaults_to_embed_document() {
        let mut provider = MockEmbeddingProvider::new(384);
        let query = provider.embed_query("test").unwrap();
        let doc = provider.embed_document("test").unwrap();
        assert_eq!(query, doc);
    }

    #[test]
    fn prefixes_produce_different_embeddings() {
        let mut provider =
            MockEmbeddingProvider::with_prefixes(384, "search_query: ", "search_document: ");

        let query = provider.embed_query("test").unwrap();
        let doc = provider.embed_document("test").unwrap();
        assert_ne!(query, doc);
    }

    #[test]
    fn batch_embed_matches_individual() {
        let mut provider = MockEmbeddingProvider::new(384);
        let texts = ["foo", "bar", "baz"];

        let batch = provider.embed_documents(&texts).unwrap();
        assert_eq!(batch.len(), 3);

        let individual: Vec<Vec<f32>> = texts
            .iter()
            .map(|t| provider.embed_document(t).unwrap())
            .collect();
        assert_eq!(batch, individual);
    }

    #[test]
    fn batch_embed_empty_input() {
        let mut provider = MockEmbeddingProvider::new(384);
        let result = provider.embed_documents(&[]).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn embed_count_tracks_calls() {
        let mut provider = MockEmbeddingProvider::new(384);
        assert_eq!(provider.embed_count, 0);

        provider.embed_document("a").unwrap();
        provider.embed_query("b").unwrap();
        assert_eq!(provider.embed_count, 2);

        provider.embed_documents(&["c", "d", "e"]).unwrap();
        assert_eq!(provider.embed_count, 5);
    }

    #[test]
    fn mock_reranker_scores_by_overlap() {
        let mut reranker = MockRerankerProvider;
        let scores = reranker
            .score_batch(
                "validate token",
                &["validate the token here", "unrelated stuff", "token"],
            )
            .unwrap();
        assert_eq!(scores.len(), 3);
        assert!(scores[0] > scores[1]);
        assert!(scores[2] > scores[1]);
    }

    #[test]
    fn mock_reranker_empty_documents() {
        let mut reranker = MockRerankerProvider;
        let scores = reranker.score_batch("test", &[]).unwrap();
        assert!(scores.is_empty());
    }
}
