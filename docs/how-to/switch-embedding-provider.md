# How to Switch Embedding Provider

> For the full `[embedding]` and `[reranker]` config key reference, see [../reference/config.md](../reference/config.md).

## Embedding Provider Configuration

Configure the embedding provider in `.cartog.toml`:

These three options are mutually exclusive — use only one `[embedding]` section.

**Option A — watcher auto-embed** (keep the local ONNX provider, control when embeddings run):

```toml
# Auto-embed under `serve --watch` / `watch`:
#   omitted / unset → auto-detect: embed only if the repo already has embeddings
#   auto_embed = true  → always auto-embed (even a never-indexed repo)
#   auto_embed = false → never auto-embed
# Precedence: CARTOG_WATCH_RAG env > this key > --rag flag.
[embedding]
auto_embed = true
```

**Option B — Ollama provider:**

```toml
[embedding]
provider = "ollama"
model = "nomic-embed-text"

[embedding.ollama]
base_url = "http://localhost:11434"
```

**Option C — OpenAI-compatible `/v1/embeddings` endpoint** (OpenAI, Mistral, Voyage, Jina, OVHcloud, or a local server like Ollama /v1, LM Studio, vLLM):

```toml
[embedding]
provider = "openai"
model    = "text-embedding-3-small"

[embedding.openai]
base_url    = "https://api.openai.com/v1"  # or http://localhost:11434/v1 (Ollama), etc.
api_key_env = "OPENAI_API_KEY"             # env var NAME, not the key itself
```

**Concurrent embedding requests (ollama/openai):**

```toml
[embedding]
max_concurrent_requests = 4   # in-flight HTTP embed requests; 1..16, default 4
```

`max_concurrent_requests` caps how many embedding requests are in flight at once
for the network providers (the local ONNX provider ignores it — it parallelizes
via `[embedding.local] intra_threads`). Default **4**, clamped `1..=16`; `1` is
serial. `CARTOG_EMBED_CONCURRENCY` overrides it (env > TOML > default). It is a
transport setting, not part of the embedding fingerprint, so changing it never
forces a re-embed. Note: the fan-out is currently gated behind a live
batch-composition parity check; until that passes, both providers run serially
regardless of this value.

**Provider options:**

| Provider | Config | Setup | Notes |
|----------|--------|-------|-------|
| `local` (default) | No config needed | `cartog rag setup` to download models | ONNX Runtime via fastembed, ~230MB models |
| `ollama` | `provider = "ollama"` | Ollama server running with model pulled | No model download needed, dimension auto-detected. Compiled into every default build; **local ONNX stays the default provider** — set `provider = "ollama"` to use it. |
| `openai` | `provider = "openai"` | Reachable OpenAI-compatible `/v1` endpoint; API key in an env var (keyless for local servers) | One generic client for OpenAI, Mistral, Voyage, Jina, OVHcloud AI Endpoints, Together/Fireworks/DeepInfra, and local `/v1` servers (Ollama, LM Studio, vLLM) — switch vendors by changing `base_url`. Dimension auto-detected. **API key read from the `api_key_env` env var, never stored in `.cartog.toml`**; unset → no auth header (keyless local). Compiled into every default build; opt in with `provider = "openai"`. Azure OpenAI is not supported (its `…/deployments/{id}/embeddings?api-version=…` path + `api-key:` header differ from the plain `/v1` + `Bearer` shape). |

**Default models (local provider):**

| Role | Config value | HuggingFace repo (downloaded) | Dim | Size |
|------|-------------|-------------------------------|-----|------|
| Embedding | `BAAI/bge-small-en-v1.5` | `Qdrant/bge-small-en-v1.5-onnx-Q` (ONNX-quantized) | 384 | ~80MB |
| Reranker | `jinaai/jina-reranker-v1-turbo-en` (default) | `jinaai/jina-reranker-v1-turbo-en` | — | ~150MB |

The embedding config value is the fastembed model code you set under `[embedding]
model`; cartog downloads the matching ONNX-quantized repo from HuggingFace into the
shared model cache (`$FASTEMBED_CACHE_DIR`, else `$XDG_CACHE_HOME/cartog/models`, else
`~/.cache/cartog/models`). English-only — non-English identifiers/comments get
degraded embeddings. Override the embedding model with any fastembed built-in via
`[embedding] model = "..."`.

An unknown `provider` value (embedding: `local`, `ollama`, `openai`; reranker: `local`, `none`) is rejected when `.cartog.toml` is loaded, with an error naming the bad value — a typo like `provider = "ollma"` fails fast instead of silently falling back to the default.

**Advanced local configuration:**

```toml
[embedding]
provider = "local"
model = "BAAI/bge-base-en-v1.5"    # any fastembed built-in model

[embedding.local]
query_prefix = "search_query: "     # for asymmetric models
document_prefix = "search_document: "
intra_threads = 4                   # cap ONNX CPU threads (default: all cores)
```

`intra_threads` **caps** the ONNX Runtime threads used while embedding
(`rag index`) and reranking. Default: **all cores** (fastembed's default); set
this to leave headroom on a busy machine (e.g. `intra_threads = 4`). The
`CARTOG_ONNX_THREADS` env var overrides it (e.g. `CARTOG_ONNX_THREADS=1`); env >
TOML > uncapped. Read at provider load, so restart `cartog serve` to change it.

**Reranker model** — the cross-encoder is configurable, mirroring `[embedding]
model`. The value is a fastembed reranker HuggingFace repo path; unset uses the
default (`jinaai/jina-reranker-v1-turbo-en`, ~150MB — small, fast, and higher
BEIR NDCG@10 than the older `bge-reranker-base`):

```toml
[reranker]
provider = "local"                              # "local" (default) | "none"
model    = "BAAI/bge-reranker-base"             # opt back to the former default (~1.1GB)
# model  = "jinaai/jina-reranker-v2-base-multilingual"  # multilingual (~300MB)
```

The reranker is not persisted, so switching models needs no re-index — the change
takes effect on the next search (a new model downloads once; a previously-used one
is reused from cache). Existing users who never pinned `model` are switched to the
new default automatically; pin `model = "BAAI/bge-reranker-base"` to keep the old
one (it reuses the already-downloaded weights). See
[troubleshooting](../troubleshooting.md) to reclaim the orphaned `bge-reranker-base`
cache.

**Disable re-ranking** (skips the ~150MB reranker download). Either spelling
works; `enabled = false` wins over an explicit `provider`:

```toml
[reranker]
enabled = false
# provider = "none"   # equivalent
```
