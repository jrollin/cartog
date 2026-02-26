# cartog

[![CI](https://github.com/jrollin/cartog/actions/workflows/ci.yml/badge.svg)](https://github.com/jrollin/cartog/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/jrollin/cartog/branch/main/graph/badge.svg)](https://codecov.io/gh/jrollin/cartog)
[![Crates.io](https://img.shields.io/crates/v/cartog.svg)](https://crates.io/crates/cartog)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Map your codebase. Navigate by graph, not grep.

cartog is a code graph indexer that gives LLM coding agents (Claude Code, Cursor, etc.) instant structural understanding of a codebase — replacing repeated grep/cat with targeted graph queries.

## Why

Code is a graph of relationships (calls, imports, inherits, type references). cartog pre-computes this graph with tree-sitter, stores it in SQLite, and lets you query it instead of re-discovering structure from scratch every time.

**Before** (6+ tool calls, ~2000 tokens, might still miss references):
```
grep -r 'validate_token' .
cat src/auth/tokens.py
cat src/services/user.py
cat src/middleware/auth.py
```

**After** (2-3 calls, ~200 tokens, complete picture):
```
cartog refs validate_token
cartog outline src/auth/tokens.py
cartog impact validate_token
```

## Install

```bash
cargo install cartog
```

## Usage

```bash
cartog index .                              # Build the graph
cartog outline src/auth/tokens.py           # File structure without reading it
cartog refs validate_token                  # Who references this? (calls, imports, inherits, types)
cartog refs validate_token --kind calls     # Filter: only call sites
cartog callees authenticate                 # What does this call?
cartog impact SessionManager --depth 3      # What breaks if I change this?
cartog hierarchy BaseService                # Inheritance tree
cartog deps src/routes/auth.py              # File-level imports
cartog stats                                # Index summary
```

All commands support `--json` for structured output.

### Example: outline

```
$ cartog outline auth/tokens.py
from datetime import datetime, timedelta  L3
from typing import Optional  L4
import hashlib  L5
class TokenError  L11-14
class ExpiredTokenError  L17-20
function generate_token(user: User, expires_in: int = 3600) -> str  L23-27
function validate_token(token: str) -> Optional[User]  L30-44
function lookup_session(token: str) -> Optional[Session]  L47-49
function refresh_token(old_token: str) -> str  L52-56
function revoke_token(token: str) -> bool  L59-65
```

### Example: impact analysis

```
$ cartog impact validate_token --depth 3
  calls  get_current_user  auth/service.py:40
  calls  refresh_token  auth/tokens.py:54
    calls  impersonate  auth/service.py:52
```

## How It Works

1. **Index** — walks your project, parses each file with tree-sitter, extracts symbols (functions, classes, methods, imports, variables) and edges (calls, imports, inherits, raises, type references)
2. **Store** — writes everything to a local `.cartog.db` SQLite file
3. **Resolve** — links edges by name with scope-aware heuristic matching (same file > same directory > unique project match)
4. **Query** — instant lookups against the pre-computed graph

Re-indexing is incremental: only files with changed content hashes are re-parsed.

## Supported Languages

| Language | Status |
|----------|--------|
| Python | Supported |
| TypeScript (.ts, .tsx) | Supported |
| JavaScript (.js, .jsx, .mjs, .cjs) | Supported |
| Rust | Supported |
| Go | Supported |
| Ruby | Supported |
| Java | Planned |

## Design Trade-offs

- **Structural, not semantic** — name-based resolution (~90% accuracy), not full type analysis. Good enough for navigation; LSP can be layered on later.
- **Zero dependencies** — single binary + SQLite file. No language server, no embedding model, no graph DB.
- **Incremental** — SHA256 hash per file, only re-indexes what changed.

## MCP Server

cartog can run as an [MCP](https://modelcontextprotocol.io/) server, exposing all 8 tools over stdio with zero context cost.

```bash
# Claude Code
claude mcp add cartog -- cartog serve

# Cursor — add to .cursor/mcp.json
# Windsurf — add to ~/.codeium/windsurf/mcp_config.json
# OpenCode — add to .opencode.json
# Zed — add to ~/.config/zed/settings.json
```

Common config (JSON):

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

See [Usage — MCP Server](docs/usage.md#mcp-server) for per-client installation details.

## Agent Skill

Install cartog as an [Agent Skill](https://agentskills.io) for Claude Code, Cursor, Copilot, and other compatible agents:

```bash
npx skills add jrollin/cartog
```

Or install manually:

```bash
cp -r skills/cartog ~/.claude/skills/
```

The skill teaches your AI agent to use cartog for code navigation instead of grep/cat. See [Claude Code Integration](docs/claude-code.md) for details.

## Documentation

- [Usage](docs/usage.md)
- [Claude Code Integration](docs/claude-code.md)
- [Product Overview](docs/product.md)
- [Technology Stack](docs/tech.md)
- [Project Structure](docs/structure.md)

## License

MIT
