# cartog-rag

Semantic search and RAG pipeline for cartog.

## Overview

Provides embedding-based search on top of the cartog graph database. Combines keyword search (FTS5/BM25) with vector similarity (sqlite-vec) and cross-encoder reranking for high-quality results.

Indexes both **code symbols** (functions, classes, methods) and **Markdown documents** (chunked by heading). Documentation and code are searchable side-by-side with the same hybrid pipeline.

## How it works

### Embedding providers

Configurable via `.cartog.toml` `[embedding]` section. Supports pluggable providers:

| Provider | Feature flag | Models | Notes |
|----------|-------------|--------|-------|
| **local** (default) | `provider-local` | Any fastembed built-in (BGE, all-MiniLM, nomic, etc.) | ONNX Runtime via fastembed, auto-downloaded from HuggingFace |
| **ollama** | `provider-ollama` | Any Ollama model (nomic-embed-text, mxbai-embed-large, etc.) | HTTP client, auto-detects dimension |

**Reranker**: BGE-reranker-base — cross-encoder that scores (query, document) pairs jointly (~1.1GB, optional, local only).

Models cached in `~/.cache/cartog/models/` (respects `FASTEMBED_CACHE_DIR` and `XDG_CACHE_HOME`).

```toml
# .cartog.toml — example with Ollama
[embedding]
provider = "ollama"
model = "nomic-embed-text"
# dimension = 768  # auto-detected if omitted

[embedding.local]
# query_prefix = "search_query: "
# document_prefix = "search_document: "
```

### Hybrid search pipeline

1. **FTS5 keyword search** — BM25 ranking over symbol names and source content
2. **Vector KNN search** — cosine similarity on configurable-dimension embeddings via sqlite-vec
3. **RRF merge** — Reciprocal Rank Fusion (k=60) combines both ranked lists
4. **Hydration** — load symbol metadata and source content from the database
5. **Cross-encoder reranking** — reranker scores top-50 candidates (if model available)
6. **Final sort** — by rerank score (if present), else RRF score, with in-degree tiebreaker

Over-retrieves `(limit * 3).max(20)` candidates from each source to improve fusion quality before applying the final limit.

### Provider lifecycle

Providers are created per-command invocation via `create_embedding_provider(config)`. The caller passes an `EmbeddingProviderConfig` (from `.cartog.toml`) and receives a boxed `EmbeddingProvider` trait object. Reranker providers follow the same pattern with `create_reranker_provider(config)`.

## Public API

| Export | Description |
|--------|-------------|
| `create_embedding_provider()` | Create a provider from config |
| `create_reranker_provider()` | Create a reranker from config ("local" or "none") |
| `EmbeddingProviderConfig` | Configuration for provider selection |
| `provider::EmbeddingProvider` | Trait for embedding backends |
| `provider::RerankerProvider` | Trait for reranker backends |
| `search::hybrid_search()` | Run the full hybrid search pipeline |
| `indexer::index_embeddings()` | Embed symbols and write vectors to DB |
| `setup::download_model()` | Download the embedding model (local provider) |
| `EMBEDDING_DIM` | Default vector dimension (384) |

## Crate dependencies

`cartog-core`, `cartog-db`
