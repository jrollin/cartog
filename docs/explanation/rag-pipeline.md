# RAG Pipeline Design

> Architecture decisions: [architecture.md](architecture.md) | Configuration: [../reference/config.md](../reference/config.md) | How-to: [../how-to/switch-embedding-provider.md](../how-to/switch-embedding-provider.md)

## Why small models

BERT attention is **O(n²) in sequence length**. Keeping input short is the single biggest performance lever — more impactful than model size, quantization, or batching.

This drives two key decisions:

1. **Small embedding model** — BGE-small-en-v1.5 quantized (`Qdrant/bge-small-en-v1.5-onnx-Q`, default, 384 dimensions). 2-3x faster than full precision with negligible quality loss for code symbol matching. Outputs are L2-normalized, enabling L2 distance in sqlite-vec (equivalent to cosine ranking). Model and dimension are configurable via `.cartog.toml`. Trade-off: English-only model — non-English identifiers/comments get degraded embeddings.

2. **AST-aware embedding text** — For code: header + signature + significant body lines (skipping blanks, comments, closing braces) up to ~200 tokens (~800 bytes):
   ```text
   // File: auth/tokens.py | function validate_token
   def validate_token(token: str) -> bool:
       if token.is_expired():
           raise TokenError('expired')
       return lookup_session(token.session_id)
   ```
   For Markdown documents: heading text in the header field + section body:
   ```text
   // File: docs/design.md
   // Type: document
   // Name: Authentication
   Users authenticate via JWT tokens. The server validates
   the token signature and checks expiration...
   ```
   This captures the "what does this function/section do" signal (~100-200 tokens) while staying within the model's 512-token window. Full source content is still stored separately for FTS5 keyword search and cross-encoder re-ranking. Decorators/annotations are kept (they carry semantic meaning like `@login_required`).

## What gets embedded (and what doesn't)

- **Functions, classes, methods**: embedded with AST-aware text (header + significant body lines)
- **Markdown documents**: chunked by heading, each section embedded with heading in header field. Large sections sub-chunked at paragraph boundaries (~1500 bytes). Files without headings use fixed-size paragraph chunking.
- **Variables**: excluded — too numerous, low signal for semantic search
- **Imports**: excluded at content extraction time — they exist as graph edges, not search targets

## Search pipeline

```text
Query
  │
  ├─→ FTS5 keyword search (BM25 ranking)
  │     3-tier fallback: phrase → AND → OR
  │     Searches: symbol_name + normalized_name + full content
  │
  ├─→ Vector KNN search (sqlite-vec, L2 distance)
  │     L2-normalized embeddings → L2 distance ≡ cosine ranking
  │     Query embedded with configured provider (default: BGE-small-en-v1.5)
  │
  ├─→ Reciprocal Rank Fusion (RRF, k=60)
  │     Merges both ranked lists: score = Σ 1/(k + rank + 1)
  │     k=60 from Cormack et al. 2009
  │     Over-retrieval: max(limit × 3, 20) per source
  │
  └─→ Cross-encoder re-ranking (optional)
        jina-reranker-v1-turbo-en (`jinaai/jina-reranker-v1-turbo-en`, default; configurable via `[reranker] model`, or disable with `[reranker] enabled = false`), scores (query, full_content) pairs jointly
        Capped at 50 candidates to bound latency
        Graceful degradation: tri-state cache (not attempted / failed / ready)
        If model unavailable → search works with RRF-only ordering
```

## FTS5 three-tier fallback (precision-first)

| Tier | Query form | Example for "validate token" |
|------|-----------|------------------------------|
| 1 | Phrase | `"validate token"` — exact adjacent match |
| 2 | AND | `"validate" AND "token"` — all terms, any order |
| 3 | OR | `"validate" OR "token"` — any term present |

Returns the first non-empty result. Only FTS5 syntax errors trigger fallback — real DB errors propagate.

## Key constants

| Constant | Value | Rationale |
|----------|-------|-----------|
| `EMBEDDING_DIM` | 384 | BGE-small-en-v1.5 output dimension (default, overridable per-provider) |
| `EMBED_BATCH_SIZE` | 64 | Limits ONNX padding waste when text lengths vary |
| `CHUNK_SIZE` | 512 | Symbols per embedding engine call |
| `DB_BATCH_LIMIT` | 256 | Pending DB writes before flush |
| `MAX_CONTENT_BYTES` | 2048 | ~512 tokens at code's char/token ratio (stored for FTS5 + reranker) |
| `MIN_CONTENT_BYTES` | 50 | Below this → noise, not embedded |
| `MAX_EMBED_TEXT_BYTES` | 800 | ~200 tokens for bi-encoder input (AST-aware significant lines) |
| `EMBEDDING_FORMAT_VERSION` | 4 | Auto-triggers re-embed when embedding strategy changes |
| `RERANK_MAX` | 50 | Cross-encoder candidate cap; configurable via `[rag] rerank_max` |
| `RERANK_MIN` | 8 | Skip the cross-encoder entirely if fewer than this many candidates survived RRF merge; configurable via `[rag] rerank_min`, capped at `RERANK_MAX` |
| RRF `k` | 60.0 | Standard constant from Cormack et al. 2009 |
| Over-retrieval | `limit × 3` (min 20) | Enough candidates for effective RRF merge |
| `MAX_SEARCH_LIMIT` | 100 | Hard cap on returned results |

## Provider architecture

Embedding providers implement the `EmbeddingProvider` trait with `embed_query()`/`embed_document()` separation for asymmetric models. Providers are selected at runtime via `.cartog.toml` (default `provider = "local"`) and gated behind Cargo feature flags (`provider-local`, `provider-ollama`, `provider-openai`). The `cartog` binary compiles all three in by default; selecting Ollama or an OpenAI-compatible endpoint is a config choice, not a rebuild. The generic `openai` provider speaks the OpenAI `/v1/embeddings` shape, so one HTTP client covers OpenAI, Mistral, Voyage, Jina, OVHcloud, and local `/v1` servers — switch vendors by changing `base_url`. (Azure OpenAI's deployment-path + `api-key:` header shape is out of scope.)

**Dimension changes are safe.** Each provider reports its embedding dimension (auto-detected via a one-shot probe at construction for remote providers, or pinned via `[embedding] dimension`). The `symbol_vec` vec0 table is sized to that dimension, and the dimension + `(provider, model)` identity are stored as a fingerprint in `metadata`. On the next RAG open, `reconcile_embedding_fingerprint` compares the live provider's fingerprint to the stored one: a full match is a zero-write no-op; **any** difference (dimension, provider, or model) atomically drops `symbol_vec`, clears `symbol_embedding_map`, recreates the table at the new dimension, and logs "fingerprint changed — run `cartog rag index` to re-embed." Stale vectors are never queried at the wrong dimension. (Switching from local 384-dim BGE to OpenAI's 1536-dim `text-embedding-3-small` triggers exactly this rebuild.) Read-only graph commands open with the static default dimension and never wipe a non-default vector index — `handle_embedding_dimension` preserves the stored dimension when the caller passes the default.

**Concurrent network embedding.** The `EmbeddingProvider` trait stays sync/blocking; the network providers fan out via a shared `providers::concurrent` runner (`std::thread::scope` work-queue over indexed sub-batches → a slot-indexed collector that reconstructs input order). `[embedding] max_concurrent_requests` (env `CARTOG_EMBED_CONCURRENCY`, default 4, clamped `1..=16`) caps in-flight requests; `1` is a serial path byte-identical to the single-request behavior. Failures return the lowest-ordinal error deterministically; transient ones (429/503/timeout/connect) retry with capped backoff + deterministic per-(ordinal, attempt) jitter (no `Retry-After` — `error_for_status()` consumes the response before the error is built). `max_concurrent_requests` is transport-only, excluded from the embedding fingerprint. The fan-out is gated behind a live batch-composition parity test per provider (Ollama returns positional, un-indexed vectors, a stronger split assumption than OpenAI's `index`-tagged array); until each gate passes, `create_embedding_provider` forces that provider to concurrency 1. Note: against a single local Ollama the fan-out gives ~1.0× (one model instance serializes inference server-side); the speedup is for remote endpoints where requests parallelize and round-trip latency dominates.
