# cartog — Technology Stack

## Language

**Rust** — Performance, tree-sitter native bindings, single binary distribution.

## Core Dependencies

| Crate | Purpose |
|-------|---------|
| `tree-sitter` | Incremental parsing, CST traversal |
| `tree-sitter-{lang}` | Per-language grammars (Python, TS/JS, Rust, Go, Java) |
| `rusqlite` (bundled) | SQLite storage, zero external deps |
| `clap` (derive) | CLI argument parsing |
| `serde` + `serde_json` | JSON serialization for `--json` output |
| `walkdir` | Recursive directory traversal |
| `sha2` | File content hashing for change detection |

## Architecture Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Parser | tree-sitter | Incremental, multi-language, structural |
| Storage | SQLite | Zero infra, ~1MB, persists across sessions |
| Packaging | Skill (primary) | Changes agent workflow, not just adds a tool |
| Change detection | Git-based + SHA256 fallback + `--force` | Minimal re-indexing, deferred file reads |
| Vector DB | No | 100MB+ model, non-deterministic, slow |
| LSP | Deferred | Tree-sitter handles 90% of cases |
| MCP server | Deferred | Skill-first approach, MCP when demanded |
| Watch mode | No | On-demand re-index is sufficient for agent use |
| Language detection | File extension | Simple, predictable, covers 99% |
| Monorepo | Deferred | Index from CWD, user can cd into subproject |
| Output format | Human default + `--json` flag | Readable for humans, parseable for scripts |
| Edge resolution | Exact name, scope-aware | Same file > same dir > same project priority |
| Distribution | `cargo install` first, binaries later | Ship fast, optimize distribution after |

## Minimum Supported Rust Version

1.75+ (edition 2021)
