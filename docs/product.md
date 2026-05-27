# cartog — Product Overview

> Strategic positioning. For installation and commands, see the [README](../README.md) and [usage.md](usage.md).

**Tagline:** Map your codebase. Navigate by graph, not grep.

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

Measured across 13 scenarios, 5 languages. Best gains on call chain tracing (88% token reduction) and caller lookup (95% reduction).

> **Recall caveat:** The 97% figure requires a matching language server on `PATH` (the default build ships LSP support). Without a server (or with `--no-lsp`), edge resolution falls to ~25–37% depending on language; with LSP it reaches 44–81%. See [README — Benchmark notes](../README.md#benchmark-notes) for methodology.

## Target users

- **LLM coding agents** — Claude Code, Cursor, Aider, Copilot, or any LLM with bash/MCP access.
- **Developers** who want fast structural navigation without running a language server.
- **Privacy-conscious teams** — local by default, no API calls, works in air-gapped environments. Opt-in S3-compatible index sync is available for teams that explicitly want to share a prebuilt index (see [usage.md](usage.md#cartog-push---remote-s3-url)).

## Differentiation

**vs grep/cat/find:** pre-computed graph eliminates multi-step discovery. One `refs` call replaces grep + filter + read. Transitive analysis (`impact`) is impossible with grep alone.

**vs language servers (LSP):** no startup time, no per-language server, no config. Single binary covers 9 languages (8 code languages + Markdown). Trade-off: ~90% name resolution accuracy vs LSP's full semantic analysis. LSP can be enabled as an optional precision layer.

**vs Serena MCP / codanna / Aider repo-map:** single binary, no LSP requirement, pre-computed graph (not per-query), full query interface over SQLite.

## Distribution

- **CLI binary** — `cargo install cartog` or `curl -fsSL https://jrollin.github.io/cartog/install.sh | sh`. Bootstrap with `cartog init` + `cartog index`.
- **MCP server** (`cartog serve`), wired into editors via `cartog ide`. Supports Claude Code, Claude Desktop, Cursor, VS Code, Codex CLI, Gemini CLI, OpenCode, Windsurf, Zed.
- **Claude Code plugin** (`/plugin install cartog@cartog-plugins`) and agent skill (`npx skills add jrollin/cartog`) for users who want the bundled install + skill + MCP setup in one step.

## Further reading

- [usage.md](usage.md) — CLI reference and configuration
- [mcp-setup.md](mcp-setup.md) — per-editor MCP wiring
- [tech.md](tech.md) — technology stack and architecture decisions
- [architecture/incremental-indexing.md](architecture/incremental-indexing.md) — how the graph stays fresh
