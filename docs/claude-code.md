# Using cartog with Claude Code

## Installation

```bash
# From source
cargo build --release
cargo install --path .

# From crates.io
cargo install cartog
```

## Setup as an Agent Skill

Install the cartog skill for Claude Code, Cursor, Copilot, and other [Agent Skills](https://agentskills.io)-compatible agents:

```bash
npx skills add jrollin/cartog
```

Or install manually:

```bash
cp -r skills/cartog ~/.claude/skills/
```

This teaches your AI agent to automatically use cartog for code navigation instead of grep/cat.

At session start, ensure the index exists:

```bash
bash skills/cartog/scripts/ensure_indexed.sh
```

## Workflow

### 1. Index your project

```bash
cartog index .
```

Creates a `.cartog.db` SQLite file with the code graph. Incremental -- re-running only processes changed files.

### 2. Query the graph instead of reading files

| Task | Command |
|------|---------|
| File structure (replaces `cat`) | `cartog outline src/main.rs` |
| Who references a symbol? | `cartog refs validate_token` |
| Only call sites? | `cartog refs validate_token --kind calls` |
| What does a function call? | `cartog callees authenticate` |
| Refactoring blast radius | `cartog impact SessionManager --depth 3` |
| Class hierarchy | `cartog hierarchy BaseService` |
| File imports | `cartog deps src/routes/auth.py` |
| Index summary | `cartog stats` |

### 3. Use `--json` for structured output

```bash
cartog --json refs validate_token
```

## What the Skill Teaches Claude Code

The skill (`skills/cartog/SKILL.md`) instructs your AI agent to:

- Use `cartog outline` **before** reading a file when structure is needed, not content
- Run `cartog refs/impact` **before** grepping for references
- Run `cartog index .` after making code changes to keep the graph fresh
- Only fall back to `cat`/`grep` for actual implementation details, string literals, or config values

## Skill Contents

| File | Purpose |
|------|---------|
| `skills/cartog/SKILL.md` | Behavioral instructions for AI agents |
| `skills/cartog/scripts/install.sh` | Automated installation via `cargo install` |
| `skills/cartog/scripts/ensure_indexed.sh` | Ensures `.cartog.db` exists and is up to date |
| `skills/cartog/scripts/query.sh` | Thin wrapper running `cartog --json "$@"` |
| `skills/cartog/references/query_cookbook.md` | Recipes for common navigation patterns |
| `skills/cartog/references/supported_languages.md` | Language support matrix |

## Why It Helps

Instead of Claude Code doing repeated `grep` and `cat` to understand code structure (6+ tool calls, ~2000 tokens), cartog pre-computes a call graph with tree-sitter and stores it in SQLite. Queries return in microseconds (2-3 calls, ~200 tokens, complete picture).

## Supported Languages

Python, TypeScript/JavaScript, Rust, and Go. Java is planned.
