# cartog documentation

Start here: the root [README](../README.md) for install and quick start.

This documentation is organized by the [Diataxis](https://diataxis.fr) framework:
**Tutorials** (learning), **How-to** (task), **Reference** (lookup), **Explanation** (understanding).

## Tutorials

- [tutorials/quickstart.md](tutorials/quickstart.md) — install, first index, bootstrap

## How-to

- [how-to/configure-lsp-servers.md](how-to/configure-lsp-servers.md) — LSP server overrides and concurrency
- [how-to/switch-embedding-provider.md](how-to/switch-embedding-provider.md) — local ONNX, Ollama, OpenAI-compatible
- [how-to/set-up-s3-sync.md](how-to/set-up-s3-sync.md) — remote push/pull via S3-compatible storage
- [how-to/update-cartog.md](how-to/update-cartog.md) — upgrade, rollback, deferred updates
- [how-to/wire-editors.md](how-to/wire-editors.md) — `cartog ide` and manual MCP config

## Reference

- [reference/cli.md](reference/cli.md) — all 27 commands and global flags
- [reference/config.md](reference/config.md) — all `.cartog.toml` keys and environment variables
- [reference/mcp-tools.md](reference/mcp-tools.md) — 16 MCP tools, progress, cancellation, logging
- [reference/exit-codes.md](reference/exit-codes.md) — `cartog self` exit codes and state file
- [updates.md](updates.md) — full `cartog self update` surface (canonical reference)

## Explanation

- [explanation/architecture.md](explanation/architecture.md) — design decisions, secret redaction, SQLite tuning
- [explanation/rag-pipeline.md](explanation/rag-pipeline.md) — hybrid search, embedding models, provider architecture
- [explanation/incremental-indexing.md](explanation/incremental-indexing.md) — three-layer change detection, Merkle invariants
- [explanation/concurrency.md](explanation/concurrency.md) — rayon / tokio / std::thread models

## Background docs

- [product.md](product.md) — vision, target users, positioning
- [tech.md](tech.md) — technology stack, dependencies, benchmarks
- [structure.md](structure.md) — workspace layout and per-crate links
- [mcp-setup.md](mcp-setup.md) — per-editor MCP wiring (Cursor, VS Code, Codex, Gemini, Claude Desktop, OpenCode, Windsurf, Zed, Antigravity, Kiro, Hermes)
- [agent-snippet.md](agent-snippet.md) — drop-in rules that teach your agent when to prefer cartog over grep
- [editor-integration.md](editor-integration.md) — Neovim, VS Code, Emacs, Zed CLI recipes
- [troubleshooting.md](troubleshooting.md) — common errors and fixes

## Release

- [release-smoke.md](../scripts/release-smoke.md) — manual smoke checklist run before tagging

## Assets

- [demo.tape](demo.tape) — VHS script used to render `demo.gif`

## See also

- [CONTRIBUTING.md](../CONTRIBUTING.md) — how to build, test, and contribute
- [AGENTS.md](../AGENTS.md) — instructions for AI agents working on this repo
- [CHANGELOG.md](../CHANGELOG.md) — release history
