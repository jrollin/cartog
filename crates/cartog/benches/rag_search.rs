//! Hybrid-search latency benchmarks for the RAG pipeline.
//!
//! Measures `hybrid_search` (FTS5 + vector KNN + RRF merge) over the indexed
//! Python fixture. Embeddings are produced by a deterministic stub provider, so
//! this bench **never loads an ONNX model** and is safe to run in CI — the same
//! approach as `tests/rag_relevancy.rs`. Real-model embed/rerank latency lives
//! in the separate `rag_onnx` bench, which CI does not run.
//!
//! The reranker is intentionally `None`: the cross-encoder requires the ONNX
//! model. Reranking latency is covered by `rag_onnx`.
//!
//! Run with: `cargo bench -p cartog --bench rag_search`

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};

use cartog::db::Database;
use cartog::indexer::{bench_support, index_directory};
use cartog::rag::indexer::index_embeddings;
use cartog::rag::provider::EmbeddingProvider;
use cartog::rag::search::{hybrid_search, KindFilter};

/// Deterministic, ONNX-free embedding provider.
///
/// Returns a unit-length vector derived from a cheap hash of the input so that
/// distinct texts get distinct vectors — the vector-search distance scan then
/// does realistic work instead of comparing identical zero vectors.
struct StubEmbeddingProvider;

impl StubEmbeddingProvider {
    const DIM: usize = 384;
}

impl EmbeddingProvider for StubEmbeddingProvider {
    fn name(&self) -> &str {
        "stub"
    }
    fn model_id(&self) -> &str {
        "stub-model"
    }
    fn dimension(&self) -> usize {
        Self::DIM
    }
    fn embed_document(&mut self, text: &str) -> anyhow::Result<Vec<f32>> {
        let mut seed: u64 = 1469598103934665603;
        for b in text.bytes() {
            seed ^= u64::from(b);
            seed = seed.wrapping_mul(1099511628211);
        }
        let mut v = vec![0.0f32; Self::DIM];
        for (i, slot) in v.iter_mut().enumerate() {
            let h = seed.wrapping_add((i as u64).wrapping_mul(0x9E3779B97F4A7C15));
            *slot = ((h >> 11) as f32 / (1u64 << 53) as f32) - 0.5;
        }
        let norm = v
            .iter()
            .map(|x| x * x)
            .sum::<f32>()
            .sqrt()
            .max(f32::EPSILON);
        for x in &mut v {
            *x /= norm;
        }
        Ok(v)
    }
    fn embed_documents(&mut self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        texts.iter().map(|t| self.embed_document(t)).collect()
    }
}

/// Index the Python fixture and embed every symbol with the stub provider.
fn setup_db() -> Database {
    let db = Database::open_memory().expect("open in-memory DB");
    index_directory(
        &db,
        &bench_support::fixture_path(),
        true,
        false,
        None,
        None,
        cartog::indexer::RedactionConfig::disabled(),
        &std::collections::HashMap::new(),
        &cartog::indexer::WalkFilter::unrestricted(),
    )
    .expect("index fixture");
    let mut provider = StubEmbeddingProvider;
    index_embeddings(&db, &mut provider, true, None, None).expect("embed fixture");
    db
}

/// Representative queries spanning short/long and keyword/conceptual phrasings.
const QUERIES: [&str; 4] = [
    "validate token",
    "database connection pool",
    "authenticate user",
    "send notification email",
];

fn bench_hybrid_search(c: &mut Criterion) {
    let db = setup_db();
    let mut provider = StubEmbeddingProvider;

    let mut group = c.benchmark_group("hybrid_search");
    for query in QUERIES {
        group.bench_with_input(BenchmarkId::from_parameter(query), &query, |b, &q| {
            b.iter(|| {
                black_box(
                    hybrid_search(
                        &db,
                        black_box(q),
                        black_box(10),
                        KindFilter::CodeOnly,
                        &mut provider,
                        None,
                    )
                    .unwrap(),
                )
            })
        });
    }
    group.finish();
}

criterion_group!(benches, bench_hybrid_search);
criterion_main!(benches);
