# Explanation

Understanding-oriented docs: the why and how behind cartog's design.

- [architecture.md](architecture.md) — design decisions, secret redaction, SQLite tuning, MSRV
- [concurrency.md](concurrency.md) — rayon / tokio / std::thread models and their interactions
- [incremental-indexing.md](incremental-indexing.md) — three-layer change detection and Merkle invariants
- [rag-pipeline.md](rag-pipeline.md) — hybrid FTS5 + vector search, RRF fusion, reranker
- [project-registry.md](project-registry.md) — user-global registry of indexed projects: `cartog projects` (shipped); `cartog_list_projects` and self-populated descriptions (proposed)
- [cross-project-queries.md](cross-project-queries.md) — *proposal* — federated search, cross-service contract edges, shared read-only server
