# Explanation

Understanding-oriented docs: the why and how behind cartog's design.

- [architecture.md](architecture.md) — design decisions, secret redaction, SQLite tuning, MSRV
- [concurrency.md](concurrency.md) — rayon / tokio / std::thread models and their interactions
- [incremental-indexing.md](incremental-indexing.md) — three-layer change detection and Merkle invariants
- [rag-pipeline.md](rag-pipeline.md) — hybrid FTS5 + vector search, RRF fusion, reranker
