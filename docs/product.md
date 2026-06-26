# cartog — Product Overview

> Strategic positioning. For installation and commands, see the [README](../README.md) and [usage.md](usage.md).

**Tagline:** Map your codebase. Navigate by graph, not grep.

**Positioning line:** Semantic search that returns named symbols, not text chunks — ranked, reranked, and budget-aware.

## What it does

cartog is a code graph indexer that gives LLM coding agents instant structural understanding of a codebase. It replaces repeated grep/cat with targeted graph queries: **83% fewer tokens per query, 97% recall**.

Code is a graph of relationships (calls, imports, inherits, type references). cartog pre-computes that graph with tree-sitter, stores it in SQLite, and lets the agent query it instead of re-discovering structure from scratch every time.

## Results

| | grep/cat workflow | cartog |
|---|---|---|
| Tokens per query | ~1,700 | ~280 (83% fewer) |
| Recall (completeness) | 78% | 97% |
| Query latency | multi-step | 8–450 µs |
| Transitive analysis | impossible | `impact --depth 3` |

Measured across 13 scenarios, 10 languages. Best gains on call chain tracing (88% token reduction) and caller lookup (95% reduction).

> **Recall caveat:** The 97% figure requires a matching language server on `PATH` (the default build ships LSP support). Without a server (or with `--no-lsp`), edge resolution falls to ~25–37% depending on language; with LSP it reaches 44–81%. See [README — Benchmark notes](../README.md#benchmark-notes) for methodology.

## Target users

- **LLM coding agents** — Claude Code, Cursor, Aider, Copilot, or any LLM with bash/MCP access.
- **Developers** who want fast structural navigation without running a language server.
- **Privacy-conscious teams** — local by default, no API calls, works in air-gapped environments. Opt-in S3-compatible index sync is available for teams that explicitly want to share a prebuilt index (see [usage.md](usage.md#cartog-push---remote-s3-url)).

## Differentiation

**vs grep/cat/find:** pre-computed graph eliminates multi-step discovery. One `refs` call replaces grep + filter + read. Transitive analysis (`impact`) is impossible with grep alone.

**vs language servers (LSP):** no startup time, no per-language server, no config. Single binary covers 15 languages (14 code languages + Markdown) plus 4 frontend frameworks (React, Vue, Svelte, Astro). Trade-off: ~90% name resolution accuracy vs LSP's full semantic analysis. LSP can be enabled as an optional precision layer.

**vs Serena MCP / codanna / Aider repo-map:** single binary, no LSP requirement, pre-computed graph (not per-query), full query interface over SQLite.

**vs embedding-search tools (chunk + vector):** tools that chunk files and embed them find code by concept but return file-and-line *chunks* — the agent still opens and reads to learn what matched, and ranking is vector-similarity only. cartog returns the **named symbol** (kind, signature, span), then adds a **cross-encoder re-ranker** and **in-degree centrality** that chunk-only ranking lacks, and **LSP-precise edges** so the same index that finds code also traces it. Embeddings run **in-process** (local ONNX) — no external Ollama/OpenAI server is required to index at all (it stays a clean opt-in). The symbol-level pipeline + AST-aware compaction also keep indexing fast: in internal benchmarking, holding the embedding backend constant, cartog's pipeline indexed materially faster than a chunk-based approach, and faster still on its default in-process ONNX.

## Distribution

- **CLI binary** — `cargo install cartog` or `curl -fsSL https://www.cartog.dev/install.sh | sh`. Bootstrap with `cartog init` + `cartog index`.
- **MCP server** (`cartog serve`), wired into editors via `cartog ide`. Supports Claude Code, Claude Desktop, Cursor, VS Code, Codex CLI, Gemini CLI, OpenCode, Windsurf, Zed, Antigravity, Kiro, Hermes Agent.
- **Claude Code plugin** (`/plugin install cartog@cartog-plugins`) and agent skill (`npx skills add jrollin/cartog`) for users who want the bundled install + skill + MCP setup in one step.

## Further reading

- [usage.md](usage.md) — CLI reference and configuration
- [mcp-setup.md](mcp-setup.md) — per-editor MCP wiring
- [tech.md](tech.md) — technology stack and architecture decisions
- [explanation/incremental-indexing.md](explanation/incremental-indexing.md) — how the graph stays fresh
