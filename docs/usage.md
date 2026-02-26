# cartog — Usage

## Setup

```bash
cargo install cartog           # from crates.io

# Or build from source:
cargo build --release
cargo install --path .
```

## Commands

### `cartog index <path>`

Build or update the graph. Run this first, then again after code changes.

```bash
cartog index .              # index current directory
cartog index src/           # index a subdirectory only
```

Incremental — skips files whose content hash hasn't changed.

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

Available `--kind` values: `function`, `class`, `method`, `variable`, `import`.

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

Available `--kind` values: `calls`, `imports`, `inherits`, `references`, `raises`.

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

File-level import graph — what does this file import?

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

### `cartog serve`

Start cartog as an MCP server over stdio. See the [MCP Server](#mcp-server) section below for client configuration.

```bash
cartog serve
```

## JSON Output

All commands accept `--json` for structured output:

```bash
cartog --json refs validate_token
cartog --json outline src/auth/tokens.py
cartog --json stats
```

## MCP Server

`cartog serve` runs cartog as an MCP server over stdio, exposing the same 9 tools for MCP-compatible clients (Claude Code, Cursor, Windsurf, etc.).

```bash
cartog serve
```

### Installation per Client

All clients need `cartog` on your `PATH` first:

```bash
cargo install cartog
```

#### Claude Code

```bash
claude mcp add cartog -- cartog serve
```

Or manually edit `~/.claude/settings.json`:

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

For project-scoped config, add to `.claude/settings.local.json` in your repo root.

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

All tool responses are JSON. The `cartog_index` tool restricts indexing to the project directory (CWD subtree).

### Logging

Logs go to stderr. Default level is `info` (server start/stop only). Set `RUST_LOG` for more detail:

```bash
RUST_LOG=debug cartog serve   # per-request tool call logging
```

### MCP vs Skill

| | MCP Server | Agent Skill |
|-|-----------|-------------|
| Context cost | Zero (tools are protocol-level) | ~150 lines of prompt |
| Workflow guidance | Basic (via `instructions` field) | Full heuristics |
| Compatibility | MCP clients only | Any LLM with bash |
| Latency | Persistent process | Fork+exec per command |

Use MCP when available for lower token cost. Use the skill for Claude.ai or non-MCP clients.
