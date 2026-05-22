# cartog documentation

Start here: the root [README](../README.md) for install and quick start.

## Guides

- [product.md](product.md) — vision, target users, positioning
- [usage.md](usage.md) — CLI reference and configuration
- [mcp-setup.md](mcp-setup.md) — per-editor MCP wiring (Cursor, VS Code, Codex, Gemini, Claude Desktop, OpenCode, Windsurf, Zed)
- [editor-integration.md](editor-integration.md) — Neovim, VS Code, Emacs, Zed CLI recipes
- [updates.md](updates.md) — `cartog self update`, exit codes, env vars, state file
- [troubleshooting.md](troubleshooting.md) — common errors and fixes

## Reference

- [tech.md](tech.md) — technology stack, architecture decisions, RAG design
- [structure.md](structure.md) — workspace layout and per-crate links
- [architecture/incremental-indexing.md](architecture/incremental-indexing.md) — layered change detection, Merkle invariants, failure modes

## Specs (design records, kept after implementation)

- [spec-watch.md](spec-watch.md) — `cartog watch` background indexer
- [spec-mcp-sharing.md](spec-mcp-sharing.md) — single-writer MCP with read-only attach
- [spec-cancellation.md](spec-cancellation.md) — cooperative cancellation for long-running MCP tools

## Release

- [release-smoke.md](release-smoke.md) — manual smoke checklist run before tagging

## Assets

- [demo.tape](demo.tape) — VHS script used to render `demo.gif`

## See also

- [CONTRIBUTING.md](../CONTRIBUTING.md) — how to build, test, and contribute
- [AGENTS.md](../AGENTS.md) — instructions for AI agents working on this repo
- [CHANGELOG.md](../CHANGELOG.md) — release history
