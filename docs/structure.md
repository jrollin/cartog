# cartog — Project Structure

## Directory Layout

```
cartog/
├── Cargo.toml
├── AGENTS.md                # Guidelines for AI coding agents
├── src/
│   ├── main.rs              # Entry point, CLI dispatch
│   ├── lib.rs               # Library root, re-exports public modules
│   ├── commands.rs          # Command handlers (outline, refs, impact, etc.)
│   ├── cli.rs               # Clap command definitions
│   ├── db.rs                # SQLite schema, CRUD, query methods
│   ├── indexer.rs           # Orchestrates: walk files → extract → store → resolve
│   ├── mcp.rs               # MCP server (tool handlers, path validation, ServerHandler)
│   ├── watch.rs             # File watcher: debounced re-index + deferred RAG embedding
│   ├── languages/
│   │   ├── mod.rs           # Language registry, Extractor trait, shared node_text helper
│   │   ├── python.rs        # Python tree-sitter extractor
│   │   ├── typescript.rs    # TypeScript/TSX extractors
│   │   ├── javascript.rs    # JavaScript extractor
│   │   ├── js_shared.rs     # Shared JS/TS extraction logic
│   │   ├── rust_lang.rs     # Rust extractor
│   │   ├── go.rs            # Go extractor
│   │   └── ruby.rs          # Ruby extractor
│   ├── rag/
│   │   ├── mod.rs           # RAG module root, constants (EMBEDDING_DIM)
│   │   ├── setup.rs         # Model download (triggers fastembed auto-download)
│   │   ├── embeddings.rs    # ONNX embedding inference via fastembed (BGE-small-en-v1.5)
│   │   ├── indexer.rs       # Embed symbols, store vectors in sqlite-vec
│   │   ├── reranker.rs      # Cross-encoder re-ranking via fastembed (BGE-reranker-base)
│   │   └── search.rs        # FTS5 + vector KNN search, RRF merge, optional re-ranking
│   └── types.rs             # Symbol, Edge, FileInfo structs
├── skills/
│   └── cartog/              # Agent Skill (agentskills.io)
│       ├── SKILL.md         # Behavioral instructions for AI agents
│       ├── scripts/
│       │   ├── install.sh
│       │   ├── ensure_indexed.sh
│       │   └── query.sh
│       ├── tests/
│       │   ├── golden_examples.yaml  # Behavioral test scenarios (expected tool calls)
│       │   ├── test_ensure_indexed.sh # Bash unit tests for ensure_indexed.sh
│       │   └── eval.sh              # LLM-as-judge evaluation via claude CLI
│       └── references/
│           ├── query_cookbook.md
│           └── supported_languages.md
├── benchmarks/
│   ├── run.sh               # Benchmark runner (token efficiency, recall, command count)
│   ├── lib/                 # Shared measurement & comparison helpers
│   ├── fixtures/
│   │   ├── webapp_py/       # Python fixture (69 files)
│   │   ├── webapp_ts/       # TypeScript fixture (48 files)
│   │   ├── webapp_go/       # Go fixture (45 files)
│   │   ├── webapp_rs/       # Rust fixture (65 files)
│   │   └── webapp_rb/       # Ruby fixture (51 files)
│   ├── ground_truth/        # Expected relationships per fixture (JSON)
│   ├── scenarios/           # 13 scenario scripts (01-13)
│   └── results/             # Benchmark output (gitignored)
├── tests/
│   ├── rag_relevancy.rs     # RAG relevancy integration benchmark (P@k, R@k, NDCG)
│   └── fixtures/
│       └── auth/            # Python fixtures for indexer tests
│           ├── tokens.py
│           └── service.py
└── docs/
    ├── product.md           # Product overview
    ├── tech.md              # Technology decisions
    ├── structure.md         # This file
    ├── usage.md             # CLI commands + MCP server setup per client
    └── claude-code.md       # Claude Code integration details
```

## Module Responsibilities

- **cli.rs**: Defines all subcommands (including `rag` subgroup and `watch`) via clap derive. No business logic.
- **db.rs**: Owns the SQLite connection. Schema creation (core + RAG tables), inserts, and all query methods. Returns domain types. RAG additions: `symbol_content` (source text), `symbol_fts` (FTS5 index), `symbol_vec` (sqlite-vec vectors), `symbol_embedding_map` (integer ID mapping).
- **indexer.rs**: Walks the file tree, delegates to language extractors, writes to db, runs edge resolution. Also stores symbol source content for RAG during indexing. Exports `is_ignored_dirname()` for reuse by the watcher.
- **commands.rs**: Command handlers for all CLI commands including `rag setup/index/search` and `watch`. Formats output (human-readable or `--json`).
- **mcp.rs**: MCP server over stdio. `CartogServer` struct with 11 `#[tool]` handlers (9 core + 2 RAG). Path validation restricts `index` to CWD subtree. Uses `spawn_blocking` for sync DB/indexer calls. Optionally spawns a background file watcher (`--watch` flag).
- **watch.rs**: File watcher using `notify-debouncer-mini`. Debounces filesystem events, triggers incremental `index_directory()`. Optionally defers RAG embedding after a configurable delay. Used standalone (`cartog watch`) or embedded in MCP server (`cartog serve --watch`).
- **languages/mod.rs**: Maps file extensions to extractors, defines the `Extractor` trait and shared `node_text` helper. Each extractor implements `fn extract(&self, source: &str, file_path: &str) -> Result<ExtractionResult>`.
- **rag/mod.rs**: RAG pipeline constants (`EMBEDDING_DIM = 384`), shared model cache directory (`model_cache_dir()` — XDG-compliant, avoids per-project model downloads).
- **rag/setup.rs**: Triggers model download by instantiating fastembed engines (models auto-downloaded from HuggingFace on first use).
- **rag/embeddings.rs**: ONNX Runtime inference via fastembed (`BAAI/bge-small-en-v1.5`). Serialization helpers for sqlite-vec byte format.
- **rag/indexer.rs**: Embeds all symbols with content, stores in sqlite-vec. Supports incremental (skip existing) and force modes.
- **rag/search.rs**: Hybrid search combining FTS5 keyword (BM25) + vector KNN (cosine), merged via Reciprocal Rank Fusion (RRF, k=60). Optional cross-encoder re-ranking when model is available.
- **rag/reranker.rs**: Cross-encoder re-ranking via fastembed (`BAAI/bge-reranker-base`). Scores (query, document) pairs jointly. Auto-enabled when model is downloadable.
- **types.rs**: Shared data structures. No logic beyond Display/serialization.

## Conventions

- No `unwrap()` in library code — use `anyhow::Result` for error propagation.
- All public functions documented with `///` doc comments.
- CLI output: human-readable by default, `--json` for structured output.
- Tests: unit tests in each module (`#[cfg(test)]`), fixture files in `tests/fixtures/` for indexer tests.
