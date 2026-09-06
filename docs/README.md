# cartog documentation

Start here: the root [README](../README.md) for install and quick start.

This documentation is organized by the [Diataxis](https://diataxis.fr) framework:
**Tutorials** (learning), **How-to** (task), **Reference** (lookup), **Explanation** (understanding).

## Tutorials

- [tutorials/quickstart.md](tutorials/quickstart.md) — install, first index, bootstrap

## How-to

- [how-to/configure-lsp-servers.md](how-to/configure-lsp-servers.md) — LSP server overrides and concurrency
- [how-to/query-another-project.md](how-to/query-another-project.md) — query another indexed project on this machine with `--db`
- [how-to/switch-embedding-provider.md](how-to/switch-embedding-provider.md) — local ONNX, Ollama, OpenAI-compatible
- [how-to/set-up-s3-sync.md](how-to/set-up-s3-sync.md) — remote push/pull via S3-compatible storage
- [how-to/update-cartog.md](how-to/update-cartog.md) — upgrade, rollback, deferred updates
- [how-to/wire-editors.md](how-to/wire-editors.md) — `cartog ide` and manual MCP config

## Reference

- [reference/cli.md](reference/cli.md) — all 28 commands and global flags
- [reference/config.md](reference/config.md) — all `.cartog.toml` keys and environment variables
- [reference/mcp-tools.md](reference/mcp-tools.md) — 16 MCP tools (+2 opt-in cross-project), progress, cancellation, logging
- [reference/exit-codes.md](reference/exit-codes.md) — `cartog self` exit codes and state file
- [updates.md](updates.md) — full `cartog self update` surface (canonical reference; procedures are in [how-to/update-cartog.md](how-to/update-cartog.md))

## Explanation

- [explanation/architecture.md](explanation/architecture.md) — design decisions, secret redaction, SQLite tuning
- [explanation/rag-pipeline.md](explanation/rag-pipeline.md) — hybrid search, embedding models, provider architecture
- [explanation/incremental-indexing.md](explanation/incremental-indexing.md) — three-layer change detection, Merkle invariants
- [explanation/concurrency.md](explanation/concurrency.md) — rayon / tokio / std::thread models
- [explanation/project-registry.md](explanation/project-registry.md) — user-global project registry: `cartog projects`, `cartog_list_projects`, and self-populated descriptions (shipped), and the proposed later phases
- [explanation/cross-project-queries.md](explanation/cross-project-queries.md) — *proposal* — federated search and cross-service edges

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

## Demo

The animated GIF on the README and the landing page is generated from `demo.tape` using
[VHS](https://github.com/charmbracelet/vhs). The tape walks through:
`cartog init` → `cartog index` → `cartog stats` → `cartog search` → `cartog rag setup`
→ `cartog rag index` → `cartog rag search`.

To regenerate after changing CLI output:

```bash
brew install vhs                        # or: go install github.com/charmbracelet/vhs@latest
cargo build --release
PATH="$PWD/target/release:$PATH" vhs docs/demo.tape
cp docs/demo.gif site/public/demo.gif   # keep site mirror in sync
```

- [demo.tape](demo.tape) — VHS script (edit this to update the demo)
- `demo.gif` — rendered output (commit alongside the tape; keep `site/public/demo.gif` in sync)

## See also

- [CONTRIBUTING.md](../CONTRIBUTING.md) — how to build, test, and contribute
- [AGENTS.md](../AGENTS.md) — instructions for AI agents working on this repo
- [CHANGELOG.md](../CHANGELOG.md) — release history
