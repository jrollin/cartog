# cartog

[![CI](https://github.com/jrollin/cartog/actions/workflows/ci.yml/badge.svg)](https://github.com/jrollin/cartog/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/jrollin/cartog/branch/main/graph/badge.svg)](https://codecov.io/gh/jrollin/cartog)
[![Crates.io](https://img.shields.io/crates/v/cartog.svg)](https://crates.io/crates/cartog)
[![Crates.io downloads](https://img.shields.io/crates/d/cartog)](https://crates.io/crates/cartog)
[![GitHub stars](https://img.shields.io/github/stars/jrollin/cartog)](https://github.com/jrollin/cartog)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**Map your codebase. Navigate by graph, not grep.**

Your AI coding agent wastes context re-discovering code structure with grep and cat on every task. cartog pre-computes the code graph — symbols, calls, imports, inheritance — so agents query structure in 2-3 calls instead of 6+, using 6x fewer tokens with complete coverage.

## Benchmarks

Measured across 12 scenarios, 5 languages ([full benchmark suite](benchmarks/)):

| | grep/cat | cartog | Improvement |
|---|---:|---:|---|
| **Tokens per query** | ~1,700 | ~280 | **83% fewer tokens** |
| **Recall** (completeness) | 78% | 97% | **finds what grep misses** |
| **Query latency** | n/a (multi-step) | 8-450 us | **instant** |

Where cartog shines most: tracing call chains (88% token reduction, 35% grep recall vs 100% cartog), finding callers (95% reduction), and type references (93% reduction).

![cartog demo](docs/demo.gif)

## Quick Start

```bash
cargo install cartog        # or download a pre-built binary (see below)
cd your-project
cartog index .               # build the graph (~95ms for 4k LOC, incremental)
cartog search validate       # find symbols by name
cartog refs validate_token   # who calls/imports/references this?
cartog impact validate_token # what breaks if I change this?
```

## Install

### From crates.io

```bash
cargo install cartog
```

### Pre-built binaries

Download from [GitHub Releases](https://github.com/jrollin/cartog/releases/latest):

```bash
# macOS (Apple Silicon)
curl -L https://github.com/jrollin/cartog/releases/latest/download/cartog-aarch64-apple-darwin.tar.gz | tar xz
sudo mv cartog /usr/local/bin/

# macOS (Intel)
curl -L https://github.com/jrollin/cartog/releases/latest/download/cartog-x86_64-apple-darwin.tar.gz | tar xz
sudo mv cartog /usr/local/bin/

# Linux (x86_64)
curl -L https://github.com/jrollin/cartog/releases/latest/download/cartog-x86_64-unknown-linux-gnu.tar.gz | tar xz
sudo mv cartog /usr/local/bin/

# Linux (ARM64)
curl -L https://github.com/jrollin/cartog/releases/latest/download/cartog-aarch64-unknown-linux-gnu.tar.gz | tar xz
sudo mv cartog /usr/local/bin/

# Windows (x86_64) — download .zip from releases page
```

## Commands

```bash
cartog index .                              # Build the graph
cartog search validate                      # Find symbols by partial name
cartog search validate --kind function      # Filter by kind
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

<details>
<summary><strong>Example outputs</strong></summary>

### outline

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

### search

```
$ cartog search validate
function  validate_token    auth/tokens.py:30
function  validate_session  auth/tokens.py:68
function  validate_user     services/user.py:12
```

Results ranked: exact match > prefix > substring. Case-insensitive.

### impact

```
$ cartog impact validate_token --depth 3
  calls  get_current_user  auth/service.py:40
  calls  refresh_token  auth/tokens.py:54
    calls  impersonate  auth/service.py:52
```

### refs

```
$ cartog refs UserService
imports  ./service  routes/auth.py:3
calls    login  routes/auth.py:15
inherits AdminService  auth/service.py:47
references  process  routes/auth.py:22
```

</details>

## How It Works

```mermaid
graph LR
    A["Source files<br/>(py, ts, rs, go, rb)"] -->|tree-sitter| B["Symbols + Edges"]
    B -->|write| C[".cartog.db<br/>(SQLite)"]
    C -->|query| D["search / refs / impact<br/>outline / callees / hierarchy"]
```

1. **Index** — walks your project, parses each file with tree-sitter, extracts symbols (functions, classes, methods, imports, variables) and edges (calls, imports, inherits, raises, type references)
2. **Store** — writes everything to a local `.cartog.db` SQLite file
3. **Resolve** — links edges by name with scope-aware heuristic matching (same file > same directory > unique project match)
4. **Query** — instant lookups against the pre-computed graph

Re-indexing is incremental: only files with changed content hashes are re-parsed.

## MCP Server

cartog can run as an [MCP](https://modelcontextprotocol.io/) server, exposing all 9 tools over stdio with zero context cost.

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

## Supported Languages

| Language | Extensions | Symbols | Edges |
|----------|-----------|---------|-------|
| Python | .py | functions, classes, methods, imports, variables | calls, imports, inherits, raises, type refs |
| TypeScript | .ts, .tsx | functions, classes, methods, imports, variables | calls, imports, inherits, type refs, new |
| JavaScript | .js, .jsx, .mjs, .cjs | functions, classes, methods, imports, variables | calls, imports, inherits, new |
| Rust | .rs | functions, structs, traits, impls, imports | calls, imports, inherits (trait impl), type refs |
| Go | .go | functions, structs, interfaces, imports | calls, imports, type refs |
| Ruby | .rb | functions, classes, modules, imports | calls, imports, inherits, raises, rescue types |
| Java | — | *Planned* | — |

## Performance

Indexing: **69 files / 4k LOC in 95ms** (Python fixture, release build). Incremental re-index skips unchanged files.

Query latency (criterion benchmarks on the same fixture):

| Query type | Latency |
|-----------|---------|
| outline | 8-14 us |
| hierarchy | 8-9 us |
| deps | 25 us |
| stats | 32 us |
| search | 81-102 us |
| callees | 177-180 us |
| refs | 258-471 us |
| impact (depth 3) | 2.7-17 ms |

## Design Trade-offs

- **Structural, not semantic** — name-based resolution (~90% accuracy), not full type analysis. Good enough for navigation; LSP can be layered on later.
- **Zero dependencies** — single binary + SQLite file. No language server, no embedding model, no graph DB.
- **Incremental** — SHA256 hash per file, only re-indexes what changed.

## Documentation

- [Usage](docs/usage.md)
- [Claude Code Integration](docs/claude-code.md)
- [Product Overview](docs/product.md)
- [Technology Stack](docs/tech.md)
- [Project Structure](docs/structure.md)

## License

MIT
