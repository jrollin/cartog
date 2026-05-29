//! Real-model embedding + reranking latency benchmarks.
//!
//! Unlike `rag_search` (deterministic stub provider), this bench loads the
//! actual fastembed/ONNX models, so it is **not run in CI**. It requires the
//! models to be present on disk (`cartog rag setup`); if a model is missing or
//! fails to load, the affected benches skip gracefully rather than panicking.
//! (To avoid a multi-hundred-MB download during a benchmark, set the fastembed
//! cache offline before running.)
//!
//! Run locally with: `make bench-onnx` (or `cargo bench -p cartog --bench rag_onnx`).

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion};

use cartog::rag::{create_default_embedding_provider, create_default_reranker_provider};

const QUERY: &str = "validate an authentication token and return the user";
const DOCS: [&str; 4] = [
    "fn validate_token(token: &str) -> Result<User> { /* ... */ }",
    "class DatabaseConnection: def __init__(self, dsn): ...",
    "def send_welcome_email(user): smtp.send(user.email, template)",
    "struct RateLimiter { window: Duration, max: u32 }",
];

fn bench_embedding(c: &mut Criterion) {
    let mut provider = match create_default_embedding_provider() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("skipping rag_onnx embedding benches: model unavailable ({e})");
            return;
        }
    };

    c.bench_function("embed_query", |b| {
        b.iter(|| black_box(provider.embed_query(black_box(QUERY)).unwrap()))
    });

    c.bench_function("embed_documents_batch", |b| {
        b.iter(|| black_box(provider.embed_documents(black_box(&DOCS)).unwrap()))
    });
}

fn bench_reranker(c: &mut Criterion) {
    let Some(mut reranker) = create_default_reranker_provider() else {
        eprintln!("skipping rag_onnx reranker bench: model unavailable (run `cartog rag setup`)");
        return;
    };

    c.bench_function("rerank_score_batch", |b| {
        b.iter(|| {
            black_box(
                reranker
                    .score_batch(black_box(QUERY), black_box(&DOCS))
                    .unwrap(),
            )
        })
    });
}

criterion_group!(benches, bench_embedding, bench_reranker);
criterion_main!(benches);
