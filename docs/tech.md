# cartog — Technology Stack

## Language

**Rust** — Performance, tree-sitter native bindings, single binary distribution.

## Core Dependencies

| Crate | Purpose |
|-------|---------|
| `tree-sitter` | Incremental parsing, CST traversal |
| `tree-sitter-{lang}` | Per-language grammars (Python, TS/JS, Rust, Go, Ruby) |
| `rusqlite` (bundled) | SQLite storage, zero external deps |
| `clap` (derive) | CLI argument parsing |
| `serde` + `serde_json` | JSON serialization for `--json` output |
| `walkdir` | Recursive directory traversal |
| `sha2` | File content hashing for change detection |
| `rmcp` | MCP server (JSON-RPC over stdio) |
| `tokio` | Async runtime for MCP server |
| `tracing` + `tracing-subscriber` | Structured logging (stderr) for MCP server |
| `fastembed` | ONNX Runtime inference for embeddings + re-ranking (wraps ort, tokenizers, hf-hub) |
| `sqlite-vec` | Vector similarity search (KNN) in SQLite |

## Architecture Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Parser | tree-sitter | Incremental, multi-language, structural |
| Storage | SQLite | Zero infra, ~1MB, persists across sessions |
| Packaging | Skill (primary) | Changes agent workflow, not just adds a tool |
| Change detection | Git-based + SHA256 fallback + `--force` | Minimal re-indexing, deferred file reads |
| Vector search | sqlite-vec (opt-in) | Embedded in SQLite, no external infra. Models downloaded via `cartog rag setup` |
| Model cache | `~/.cache/cartog/models` | XDG-compliant shared cache. Precedence: `FASTEMBED_CACHE_DIR` > `XDG_CACHE_HOME/cartog/models` > `~/.cache/cartog/models` |
| LSP | Deferred | Tree-sitter handles 90% of cases |
| MCP server | `cartog serve` (stdio) | Skill remains primary; MCP as secondary for zero-context-cost tool access |
| Watch mode | No | On-demand re-index is sufficient for agent use |
| Language detection | File extension | Simple, predictable, covers 99% |
| Monorepo | Deferred | Index from CWD, user can cd into subproject |
| Output format | Human default + `--json` flag | Readable for humans, parseable for scripts |
| Edge resolution | Exact name, scope-aware | Same file > same dir > same project priority |
| Distribution | `cargo install` + pre-built binaries | GitHub Releases for 5 targets, crates.io publish |

## Minimum Supported Rust Version

1.70+ (edition 2021)
