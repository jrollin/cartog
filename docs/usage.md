# cartog — Usage

## Setup

Requires Rust 1.77+ (`rustup update` if needed).

```bash
cargo install cartog           # from crates.io

# Or build from source:
cargo build --release
cargo install --path .
```

## Configuration

cartog resolves the database path using the following priority (highest wins):

| Priority | Source | Example |
|----------|--------|---------|
| 1 | `--db` flag or `CARTOG_DB` env var | `cartog --db /tmp/proj.db index .` |
| 2 | `.cartog.toml` in the project | `[database]\npath = "..."` |
| 3 | Auto git-root detection | DB placed at the root of the git repo |
| 4 | Current directory fallback | `.cartog.db` in cwd |

### Project config: `.cartog.toml`

Place `.cartog.toml` at the root of your project (or commit it to version control):

```toml
[database]
# Absolute path, or use ~ for home directory expansion
path = "~/.local/share/cartog/myproject.db"
```

This is useful when:
- Indexing from a parent directory that contains multiple projects
- Storing the DB outside the repo (e.g., to avoid committing it)
- Sharing a consistent DB location across team members via `.cartog.toml`

### Override examples

```bash
# Explicit flag (highest priority)
cartog --db /tmp/myproj.db index .
cartog --db /tmp/myproj.db search foo

# Environment variable
CARTOG_DB=~/.local/share/cartog/myproject.db cartog index .

# --db applies globally to all subcommands
cartog --db /tmp/x.db stats
cartog --db /tmp/x.db map
```

### Embedding Provider Configuration

Configure the embedding provider in `.cartog.toml`:

```toml
# Default: local ONNX model (no config needed)

# Use Ollama instead of local ONNX
[embedding]
provider = "ollama"
model = "nomic-embed-text"

[embedding.ollama]
base_url = "http://localhost:11434"
```

**Provider options:**

| Provider | Config | Setup | Notes |
|----------|--------|-------|-------|
| `local` (default) | No config needed | `cartog rag setup` to download models | ONNX Runtime via fastembed, ~1.2GB models |
| `ollama` | `provider = "ollama"` | Ollama server running with model pulled | No model download needed, dimension auto-detected |

**Advanced local configuration:**

```toml
[embedding]
provider = "local"
model = "BAAI/bge-base-en-v1.5"    # any fastembed built-in model

[embedding.local]
query_prefix = "search_query: "     # for asymmetric models
document_prefix = "search_document: "
```

**Disable re-ranking** (saves ~1.1GB model download):

```toml
[reranker]
provider = "none"
```

**Compile-time feature flags** (for minimal binary size):

```bash
cargo install cartog                                    # default: local ONNX provider
cargo install cartog --features ollama-embedding         # + Ollama support
```

---

## Commands

### `cartog index <path>`

Build or update the graph. Run this first, then again after code changes.

```bash
cartog index .              # index current directory
cartog index src/           # index a subdirectory only
cartog index . --force      # full re-index, bypassing change detection
```

Incremental by default — skips unchanged files (git diff + SHA-256), and within changed files, uses Merkle-tree diffing to update only modified symbols. Stable symbol IDs (`file:kind:qualified_name`) survive line movements, so edges from unchanged files remain valid. Use `--force` when results seem stale or after updating cartog itself.

### `cartog search <query> [--kind <kind>] [--file <path>] [--limit N]`

Find symbols by partial name — use this when you know roughly what you're looking for but need the exact name before calling `refs`, `callees`, or `impact`.

```bash
cartog search validate                       # prefix + substring match
cartog search validate --kind function       # functions only
cartog search config --file src/db.rs        # scoped to one file
cartog search parse --limit 5               # cap results
```

```
function  validate_token    auth/tokens.py:30
function  validate_session  auth/tokens.py:68
function  validate_user     services/user.py:12
```

Results ranked: exact match → prefix → substring. Case-insensitive. Max 100 results.

Available `--kind` values: `function`, `class`, `method`, `variable`, `import`, `interface`, `enum`, `type-alias`, `trait`, `module`, `document`.

### `cartog outline <file>`

Show all symbols in a file with their types, signatures, and line ranges. Use this instead of reading a file when you need structure.

```bash
cartog outline src/db.rs
```

```
use anyhow  L1
use rusqlite  L2
class Database  L62-500
  method open(path: &str) -> Result<Self>  L64-72
  method insert_symbol(&self, sym: &Symbol) -> Result<()>  L130-148
  ...
```

### `cartog callees <name>`

Find what a function calls — answers "what does this depend on?".

```bash
cartog callees validate_token
```

```
lookup_session  auth/tokens.py:37
TokenError      auth/tokens.py:39
ExpiredTokenError  auth/tokens.py:42
```

### `cartog impact <name> [--depth N]`

Transitive impact analysis — follows the caller chain up to N hops (default 3). Answers "what breaks if I change this?".

```bash
cartog impact validate_token --depth 3
```

```
  calls  get_current_user  auth/service.py:40
  calls  refresh_token  auth/tokens.py:54
    calls  impersonate  auth/service.py:52
```

Indentation shows depth.

### `cartog refs <name> [--kind <kind>]`

All references to a symbol (calls, imports, inherits, type references, raises). Optionally filter by edge kind.

```bash
cartog refs UserService                  # all reference types
cartog refs validate_token --kind calls  # only call sites
```

```
imports  ./service  routes/auth.py:3
calls    login  routes/auth.py:15
inherits AdminService  auth/service.py:47
references  process  routes/auth.py:22
```

Available `--kind` values: `calls`, `imports`, `inherits`, `references`, `raises`, `implements`, `type-of`.

### `cartog hierarchy <class>`

Show inheritance relationships involving a class — both parents and children.

```bash
cartog hierarchy AuthService
```

```
AuthService -> BaseService
AdminService -> AuthService
```

### `cartog deps <file>`

List symbols imported by a file — answers "what does this file depend on?".

```bash
cartog deps src/routes/auth.py
```

```
validate_token  L5
generate_token  L5
User            L6
```

### `cartog stats`

Summary of the index — file count, symbol count, edge resolution rate.

```bash
cartog stats
```

```
Files:    42
Symbols:  387
Edges:    1204 (891 resolved)
Languages:
  python: 30 files
  typescript: 12 files
Symbols by kind:
  function: 142
  method: 98
  class: 45
  import: 62
  variable: 40
```

### `cartog map [--tokens N]`

Token-budget-aware codebase summary — file tree + top symbols ranked by reference count (in-degree centrality).

```bash
cartog map                    # default 4000 tokens
cartog map --tokens 2000      # compact summary
cartog map --tokens 8000      # detailed summary
```

```
# Codebase Map (42 files)

  src/auth/tokens.py
  src/auth/service.py
  ...

# Top Symbols (by reference count)

src/auth/tokens.py:
  function validate_token(token: str) -> bool  L30-45  (15 refs)
  function generate_token(user_id: int) -> str  L10-28  (9 refs)
src/auth/service.py:
  class AuthService  L8-95  (12 refs)
  ...
```

Phase 1 shows the file tree; phase 2 fills remaining budget with symbols ordered by centrality (most-referenced first). Use `--json` for structured output.

### `cartog changes [--commits N] [--kind <kind>]`

Show symbols affected by recent git changes — answers "what code changed recently?".

```bash
cartog changes                        # last 5 commits + working tree
cartog changes --commits 10           # last 10 commits
cartog changes --kind function        # only functions that changed
```

```
27 files changed in last 5 commits, 158 symbols affected

src/commands.rs:
  function open_db() -> Result<Database>  L14-16
  function cmd_index(path: &str, force: bool, json: bool) -> Result<()>  L62-75
  ...

2 changed files not in index:
  Cargo.lock
  .gitignore
```

Symbols are grouped by file. Files changed but not indexed (e.g., lock files, config) are listed separately. Markdown files (`.md`) are now indexed as document sections.

### `cartog watch [path] [--debounce N] [--rag] [--rag-delay N]`

Watch for file changes and auto-re-index. Keeps the code graph fresh during development.

```bash
cartog watch                          # watch CWD, code graph only
cartog watch src/                     # watch subdirectory
cartog watch --rag                    # also auto-embed for semantic search
cartog watch --rag --rag-delay 60     # embed after 60s of inactivity
cartog watch --debounce 5             # 5s debounce window
```

The watcher runs an initial incremental index on startup, then re-indexes when supported source files change. Changes are debounced (default 2s) to avoid re-indexing on every keystroke.

When `--rag` is enabled, embedding generation is deferred until `--rag-delay` seconds (default 30) have elapsed without new file changes, batching all pending symbols in one pass.

Press Ctrl+C to stop. Pending RAG embeddings are flushed before exit.

### `cartog serve [--watch] [--rag]`

Start cartog as an MCP server over stdio. See the [MCP Server](#mcp-server) section below for client configuration.

```bash
cartog serve                  # MCP server only
cartog serve --watch          # MCP server + background file watcher
cartog serve --watch --rag    # MCP server + watcher + auto RAG embedding
```

When `--watch` is passed, a background file watcher keeps the code graph up to date as you edit. The MCP server and watcher share the same SQLite database via WAL mode (concurrent readers are safe).

### `cartog rag setup`

Download embedding and re-ranker models from HuggingFace. Run once before using RAG search.

```bash
cartog rag setup
```

**First-time download**: ~1.2GB of ONNX models (embedding ~80MB + reranker ~1.1GB). May take a few minutes depending on network speed. Models are cached in `~/.cache/cartog/models/` and reused across all projects — subsequent runs are instant.

Note: `rag setup` downloads models for the **local** provider only. When using Ollama, models are managed by the Ollama server — run `ollama pull nomic-embed-text` instead.

### `cartog rag index [path] [--force]`

Build the embedding index for semantic search. Requires `cartog index` and `cartog rag setup` first. Indexes both code symbols and Markdown documents (`.md` files).

```bash
cartog rag index              # embed all symbols + documents in CWD
cartog rag index src/         # embed a subdirectory
cartog rag index --force      # re-embed everything
```

After a cartog upgrade that changes the embedding strategy, `cartog rag index` automatically detects the format change and re-embeds all symbols — no `--force` needed.

### `cartog rag search <query> [--kind <kind>] [--limit N]`

Semantic search over code and documentation — use natural language to find code by what it does, or search project docs alongside code.

```bash
cartog rag search "validate authentication tokens"
cartog rag search "error handling" --kind function
cartog rag search "database connection" --limit 5
cartog rag search "deployment architecture" --kind document
```

Combines keyword (BM25/FTS5) and vector similarity search, merged via RRF, then re-ranked by a cross-encoder model. By default, returns code only; use `--kind document` for docs or `--kind all` for both.

Available `--kind` values: `function`, `class`, `method`, `variable`, `import`, `interface`, `enum`, `type-alias`, `trait`, `module`, `document`, `all`.

## Recommended Workflow

```
cartog index .          # 1. build the graph
cartog search foo       # 2. discover exact symbol names
cartog refs foo         # 3. find all usages
cartog callees foo      # 4. see what it depends on
cartog impact foo       # 5. assess blast radius before changing
cartog index .          # 6. re-index after code changes
```

For semantic search, add the RAG pipeline:

```
cartog rag setup        # one-time model download (~1.2GB, may take a few minutes)
cartog rag index        # embed symbols
cartog rag search "..."  # natural language queries
```

## Token Budget

All query commands accept `--tokens N` to limit human-readable output to approximately N tokens. Useful for LLM agents with limited context windows.

```bash
cartog --tokens 500 search validate        # cap output to ~500 tokens
cartog --tokens 200 outline src/db.rs      # compact file outline
cartog --tokens 1000 changes --commits 10  # budget-aware recent changes
```

Uses a `len / 4` byte-to-token approximation. Output is truncated at a character boundary with a notice. Ignored when `--json` is used (agents handle their own truncation with `--limit`).

## JSON Output

All commands accept `--json` for structured output. The flag can go before or after the subcommand:

```bash
cartog --json refs validate_token
cartog refs validate_token --json    # equivalent
cartog --json outline src/auth/tokens.py
cartog --json stats
```

Returns arrays of objects with fields like `name`, `kind`, `file_path`, `start_line`, `end_line`, `signature`, etc. Empty results return `[]`.

**Errors**: if the index doesn't exist yet, query commands print an error message and exit with a non-zero status. Run `cartog index .` first. If a symbol or file isn't found, the result is an empty array (not an error).

## Claude Plugin

cartog is available as a [Claude Code plugin](https://docs.anthropic.com/en/docs/claude-code) — the recommended way to install for Claude Code users. Plugins bundle skills, scripts, and MCP server configuration into a single installable package.

### Installation

```bash
# Register the marketplace
/plugin marketplace add jrollin/cartog

# Install the plugin
/plugin install cartog@cartog-plugins
```

This installs the plugin from GitHub, which includes:

- The agent skill (behavioral instructions in `skills/cartog/SKILL.md`)
- Setup and install scripts
- Plugin manifest (`.claude-plugin/plugin.json`)

### Plugin Structure

```
.claude-plugin/
└── plugin.json          # Plugin manifest
skills/
└── cartog/
    ├── SKILL.md         # Agent skill instructions
    ├── scripts/         # install.sh, ensure_indexed.sh
    ├── tests/           # Behavioral evals
    └── references/      # Query cookbook, language support
```

## Agent Skill

cartog also ships as an [Agent Skill](https://agentskills.io) — behavioral instructions that teach your AI agent *when and how* to use cartog, including search routing, refactoring workflows, and fallback heuristics. Use this method for non-Claude Code environments or any LLM with bash access.

### Installation

```bash
npx skills add jrollin/cartog
```

Or install manually:

```bash
cp -r skills/cartog ~/.claude/skills/
```

At session start, run the setup script (version check + 3-phase: blocking index + model download, background RAG embedding). Checks for newer cartog versions (cached, at most once per 24h). First run downloads ~1.2GB of ONNX models and may take a few minutes — subsequent runs are instant:

```bash
bash scripts/ensure_indexed.sh
```

### Skill Contents

| File | Purpose |
|------|---------|
| [`SKILL.md`](../skills/cartog/SKILL.md) | Behavioral instructions, commands, and workflows |
| [`scripts/install.sh`](../skills/cartog/scripts/install.sh) | Automated installation (pre-built binary or cargo install), accepts optional version arg |
| [`scripts/ensure_indexed.sh`](../skills/cartog/scripts/ensure_indexed.sh) | Version check + 3-phase setup: blocking index + rag setup, background rag index |
| [`tests/golden_examples.yaml`](../skills/cartog/tests/golden_examples.yaml) | Behavioral test scenarios (expected tool calls per query) |
| [`tests/test_ensure_indexed.sh`](../skills/cartog/tests/test_ensure_indexed.sh) | Bash unit tests for ensure_indexed.sh |
| [`tests/test_install.sh`](../skills/cartog/tests/test_install.sh) | Bash unit tests for install.sh |
| [`tests/eval.sh`](../skills/cartog/tests/eval.sh) | LLM-as-judge evaluation via `claude` CLI |
| [`references/query_cookbook.md`](../skills/cartog/references/query_cookbook.md) | Recipes for common navigation patterns |
| [`references/supported_languages.md`](../skills/cartog/references/supported_languages.md) | Language support matrix |

## MCP Server

`cartog serve` runs cartog as an MCP server over stdio, exposing 12 tools (10 core + 2 RAG) for MCP-compatible clients (Claude Code, Cursor, Windsurf, etc.).

```bash
cartog serve                  # basic MCP server
cartog serve --watch          # auto-re-index on file changes
cartog serve --watch --rag    # auto-re-index + auto-embed
```

### Installation per Client

All clients need `cartog` on your `PATH` first:

```bash
cargo install cartog            # latest version
cargo install cartog@0.8.1      # specific version
```

#### Claude Code

```bash
claude mcp add cartog -- cartog serve --watch
```

Or manually edit `~/.claude/settings.json`:

```json
{
  "mcpServers": {
    "cartog": {
      "command": "cartog",
      "args": ["serve", "--watch"]
    }
  }
}
```

For project-scoped config, add to `.claude/settings.local.json` in your repo root. Add `"--rag"` to args if you want automatic embedding updates.

#### Claude Desktop

Edit `~/Library/Application Support/Claude/claude_desktop_config.json` (macOS) or `%APPDATA%\Claude\claude_desktop_config.json` (Windows):

```json
{
  "mcpServers": {
    "cartog": {
      "command": "cartog",
      "args": ["serve"]
    }
  }
}
```

Restart Claude Desktop after editing.

#### Cursor

Open Settings > MCP Servers > Add Server:

- **Name**: `cartog`
- **Type**: `command`
- **Command**: `cartog serve`

Or edit `.cursor/mcp.json` in your project root:

```json
{
  "mcpServers": {
    "cartog": {
      "command": "cartog",
      "args": ["serve"]
    }
  }
}
```

#### Windsurf

Edit `~/.codeium/windsurf/mcp_config.json`:

```json
{
  "mcpServers": {
    "cartog": {
      "command": "cartog",
      "args": ["serve"]
    }
  }
}
```

#### OpenCode

Edit `~/.config/opencode/config.json` or your project `.opencode.json`:

```json
{
  "mcp": {
    "cartog": {
      "type": "stdio",
      "command": "cartog",
      "args": ["serve"]
    }
  }
}
```

#### Zed

Edit `~/.config/zed/settings.json`:

```json
{
  "context_servers": {
    "cartog": {
      "command": {
        "path": "cartog",
        "args": ["serve"]
      }
    }
  }
}
```

#### Any MCP-compatible client

The config pattern is always the same — point the client at `cartog serve` over stdio:

- **Command**: `cartog`
- **Args**: `["serve"]`
- **Transport**: stdio (default)

### Available Tools

| Tool | Parameters | Description |
|------|-----------|-------------|
| `cartog_index` | `path?`, `force?` | Build/update the code graph |
| `cartog_search` | `query`, `kind?`, `file?`, `limit?` | Find symbols by partial name |
| `cartog_outline` | `file` | File structure (symbols, line ranges) |
| `cartog_refs` | `name`, `kind?` | All references to a symbol |
| `cartog_callees` | `name` | What a symbol calls |
| `cartog_impact` | `name`, `depth?` | Transitive impact analysis |
| `cartog_hierarchy` | `name` | Inheritance tree |
| `cartog_deps` | `file` | File-level imports |
| `cartog_stats` | — | Index summary |
| `cartog_changes` | `commits?`, `kind?` | Symbols affected by recent git changes |
| `cartog_rag_index` | `path?`, `force?` | Build embedding index for semantic search |
| `cartog_rag_search` | `query`, `kind?`, `limit?` | Semantic search (FTS5 + vector + re-ranking) |

All tool responses are JSON.

**Path restriction**: `cartog_index` and `cartog_rag_index` reject paths outside the project directory (CWD subtree). Agents cannot index arbitrary filesystem locations.

### Built-in Workflow Guidance

The MCP server sends workflow instructions to the client at initialization, covering tool chaining order (index → search → refs/callees/impact → re-index) and when to use semantic search. Clients that support the MCP `instructions` field will surface these automatically.

### Logging

Logs go to stderr. Default level is `info` (server start/stop only). Set `RUST_LOG` for more detail:

```bash
RUST_LOG=debug cartog serve   # per-request tool call logging
```

### Plugin vs MCP vs Skill

| | Claude Plugin | MCP Server | Agent Skill |
|-|--------------|-----------|-------------|
| Install | `/plugin marketplace add jrollin/cartog` then `/plugin install cartog@cartog-plugins` | `claude mcp add cartog -- cartog serve` | `npx skills add jrollin/cartog` |
| Context cost | ~150 lines of prompt | Zero (tools are protocol-level) | ~150 lines of prompt |
| Workflow guidance | Full heuristics | Basic (via `instructions` field) | Full heuristics |
| Compatibility | Claude Code only | MCP clients only | Any LLM with bash |
| Latency | Fork+exec per command | Persistent process | Fork+exec per command |

Use the **plugin** for Claude Code (simplest setup, includes skill + scripts). Use **MCP** when you want lower token cost with an MCP-compatible client. Use the **skill** for non-Claude Code environments.
