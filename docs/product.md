# cartog — Product Overview

**Tagline:** Map your codebase. Navigate by graph, not grep.

## Purpose

cartog is a code graph indexer that gives LLM coding agents instant structural understanding of a codebase. It replaces repeated grep/cat with targeted graph queries.

## Core Insight

Code is a graph of relationships (calls, imports, inherits, type references). Pre-compute this graph with tree-sitter, store it in SQLite, and let the agent query it instead of re-discovering structure from scratch every time.

## Target Users

- LLM coding agents (Claude Code, Cursor, Aider, etc.)
- Developers who want fast structural navigation

## Key Features

- **Zero dependencies**: Single binary + SQLite file. No language server, no graph DB.
- **Works everywhere**: Claude.ai (as skill), Claude Code (as skill or MCP), any LLM with bash access.
- **Instant queries**: Pre-computed graph, microsecond lookups.
- **Incremental indexing**: Git-based change detection, only re-indexes modified files.
- **Semantic search (opt-in)**: Hybrid FTS5 keyword + vector similarity search over code symbols, with optional cross-encoder re-ranking. ONNX Runtime inference via fastembed. Models auto-downloaded via `cartog rag setup`.

## Differentiation

| vs Serena MCP | vs codanna | vs Aider |
|---------------|-----------|----------|
| No LSP process needed | Optional embedding model | Pre-computed graph, not per-query |
| Works in claude.ai | No MCP server required | SQLite vs in-memory NetworkX |
| Single binary | Deterministic + semantic results | Full query interface |

## Trade-off

Structural/heuristic name resolution, not full semantic. 90% accuracy — enough for most navigation tasks. LSP can be added as optional precision layer later. Semantic search adds neural embeddings for natural language queries over code.

## Distribution

- **Primary**: Claude Code skill (SKILL.md + bash scripts)
- **Secondary**: MCP server (`cartog serve` over stdio) for Claude Code, Cursor, and other MCP clients
- **Tertiary**: `cargo install cartog` / pre-built binaries for direct CLI use
