# cartog — Quick Start

> Full CLI reference: [../reference/cli.md](../reference/cli.md) | Configuration: [../reference/config.md](../reference/config.md)

## Setup

Three install paths, pick whichever matches your environment:

```bash
# 1. Install script (macOS / Linux, no Rust required) — detects platform,
#    downloads + verifies the matching release binary, installs to
#    /usr/local/bin or ~/.local/bin.
curl -fsSL https://jrollin.github.io/cartog/install.sh | sh

# 2. Cargo (Rust 1.80+; lets advanced users strip features, e.g. --no-default-features).
cargo install cartog

# 3. Build from source.
cargo build --release && cargo install --path .
```

Override the install location with `CARTOG_INSTALL_DIR=...`; pin a version
with `CARTOG_VERSION=<version>` (e.g. the tag from [Releases](https://github.com/jrollin/cartog/releases)). To upgrade an existing install in place,
`cartog self update`. See [../how-to/update-cartog.md](../how-to/update-cartog.md) for the full `cartog self`
command surface, env vars, and rollback.

## Bootstrap

From the repo root, two commands to start. The third is optional.

```bash
cargo install cartog          # one-time, global
cartog init                   # 1. scaffold .cartog.toml (config only)
cartog index                  # 2. build the code graph

cartog ide                    # optional — wire MCP into installed editors
```

| Verb | When you need it | Files |
|---|---|---|
| `cartog init` | Always (once per project) | `.cartog.toml` only |
| `cartog index` | Always (after every code change) | `.cartog/db.sqlite` |
| `cartog ide` | Only if you want MCP in your editor | `.mcp.json`, `.cursor/mcp.json`, `.vscode/mcp.json`, user-scope configs |

Edit `.cartog.toml` between steps 1 and 2 to change the DB path or embedding
provider before any heavy work runs. CLI-only users stop after step 2.

All three commands are idempotent. `cartog init` never overwrites an existing
`.cartog.toml`; `cartog ide` merges entries instead of clobbering (other MCP
servers in the file are preserved); `cartog index` is incremental.

## Next steps

- [../reference/cli.md](../reference/cli.md) — full command and flag reference
- [../reference/config.md](../reference/config.md) — all `.cartog.toml` keys and environment variables
- [../how-to/configure-lsp-servers.md](../how-to/configure-lsp-servers.md) — configure LSP servers for edge resolution
- [../explanation/rag-pipeline.md](../explanation/rag-pipeline.md) — how semantic search works (FTS5 + vector + RRF + reranker)
