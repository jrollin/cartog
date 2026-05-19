# cartog — Product Overview

> This document covers strategic positioning, target users, and outcomes. For installation and commands, see the [README](../README.md). For the full CLI reference, see [usage.md](usage.md).

**Tagline:** Map your codebase. Navigate by graph, not grep.

## Purpose

cartog is a code graph indexer that gives LLM coding agents instant structural understanding of a codebase. It replaces repeated grep/cat with targeted graph queries — **83% fewer tokens per query, 97% recall**.

## Core Insight

Code is a graph of relationships (calls, imports, inherits, type references). Pre-compute this graph with tree-sitter, store it in SQLite, and let the agent query it instead of re-discovering structure from scratch every time.

## Results

| | grep/cat workflow | cartog |
|---|---|---|
| **Tokens per query** | ~1,700 | ~280 (83% fewer) |
| **Recall** (completeness) | 78% | 97% |
| **Query latency** | multi-step | 8-450 us |
| **Transitive analysis** | impossible | `impact --depth 3` |

Measured across 13 scenarios, 5 languages. Best gains on call chain tracing (88% token reduction) and caller lookup (95% reduction).

> **Recall caveat:** The 97% figure requires a matching language server on `PATH`. The default build ships LSP support; without a server found (or with `--no-lsp`), edge resolution falls to ~25–37% depending on language; with LSP it reaches 44–81%. See [README — Benchmark notes](../README.md#benchmark-notes) for methodology and per-language breakdown.

## Target Users

- **LLM coding agents** — Claude Code, Cursor, Aider, Copilot, or any LLM with bash/MCP access
- **Developers** who want fast structural navigation without running a language server
- **Privacy-conscious teams** — fully local, no API calls, works in air-gapped environments

## Key Features

- **Zero dependencies**: Single binary + SQLite file. No language server, no graph DB.
- **Works everywhere**: Claude.ai (as skill), Claude Code (as skill or MCP), any LLM with bash access.
- **Instant queries**: Pre-computed graph — 8us for outline, 100us for search, 450us for refs.
- **Incremental indexing**: Layered change detection — git diff, file SHA-256, symbol Merkle. Only re-indexes modified files, and within those files, only updates changed symbols. See [architecture/incremental-indexing.md](architecture/incremental-indexing.md).
- **Live index**: `cartog watch` auto re-indexes on file changes. Agent always queries fresh data.
- **MCP server**: `cartog serve` exposes 13 tools over stdio. Plug into any MCP-compatible client.
- **100% local**: tree-sitter parsing, SQLite storage, ONNX embeddings. No API keys, no telemetry. Code never leaves your machine.
- **Dual search**: keyword search (sub-ms, symbol names) + semantic search (natural language, ~300ms). Run both when unsure.
- **Document indexing**: Markdown files (`.md`) are indexed alongside code. Use `--kind document` for docs or `--kind all` for both code and docs.

## Differentiation

**vs grep/cat/find (the status quo):**
Pre-computed graph eliminates multi-step discovery. One `refs` call replaces grep + filter + read. Transitive analysis (`impact`) is impossible with grep.

**vs language servers (LSP):**
No startup time, no per-language server process, no config. Single binary covers 9 languages (8 code languages plus Markdown documents). Trade-off: ~90% name resolution accuracy vs LSP's full semantic analysis.

**vs alternatives (Serena MCP, codanna, Aider repo-map):**

| vs Serena MCP | vs codanna | vs Aider |
|---------------|-----------|----------|
| No LSP process needed | Optional embedding model | Pre-computed graph, not per-query |
| Works in claude.ai | No MCP server required | SQLite vs in-memory NetworkX |
| Single binary | Deterministic + semantic results | Full query interface |

## Trade-off

Structural/heuristic name resolution, not full semantic. 90% accuracy — enough for most navigation tasks. LSP can be added as optional precision layer later. Semantic search adds neural embeddings for natural language queries over code.

## Distribution

- **Primary**: CLI binary — `cargo install cartog` or `curl -fsSL https://jrollin.github.io/cartog/install.sh | sh`. Bootstrap with `cartog init` + `cartog index`.
- **Secondary**: MCP server (`cartog serve` over stdio), wired into editors via `cartog ide`. Supports Claude Code, Claude Desktop, Cursor, VS Code, Codex CLI, Gemini CLI, OpenCode, Windsurf, Zed.
- **Tertiary**: Claude Code plugin (`/plugin install cartog@cartog-plugins`) and agent skill (`npx skills add jrollin/cartog`) for users who want the bundled install + skill + MCP setup in one step.

## Further Reading

- [Usage](usage.md) — CLI commands, agent skill setup, MCP server per client
- [Technology Stack](tech.md) — architecture decisions, dependencies
- [Project Structure](structure.md) — module layout, conventions
