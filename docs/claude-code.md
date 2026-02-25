# Using cartog with Claude Code

## Installation

```bash
# From source
cargo build --release
cargo install --path .

# From crates.io
cargo install cartog
```

## Setup as a Claude Code Skill

The `skill/` directory contains a ready-made Claude Code skill. Register it by adding the skill path to your Claude Code configuration pointing to `skill/SKILL.md`.

This teaches Claude Code to automatically use cartog for code navigation instead of grep/cat.

At session start, ensure the index exists:

```bash
bash skill/scripts/ensure_indexed.sh
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
| Who calls a function? | `cartog callers validate_token` |
| What does a function call? | `cartog callees authenticate` |
| Refactoring blast radius | `cartog impact SessionManager --depth 3` |
| All references | `cartog refs parse_config` |
| Class hierarchy | `cartog hierarchy BaseService` |
| File imports | `cartog deps src/routes/auth.py` |
| Index summary | `cartog stats` |

### 3. Use `--json` for structured output

```bash
cartog --json callers validate_token
```

## What the Skill Teaches Claude Code

The skill (`skill/SKILL.md`) instructs Claude Code to:

- Use `cartog outline` **before** reading a file when structure is needed, not content
- Run `cartog callers/impact` **before** grepping for references
- Run `cartog index .` after making code changes to keep the graph fresh
- Only fall back to `cat`/`grep` for actual implementation details, string literals, or config values

## Skill Contents

| File | Purpose |
|------|---------|
| `skill/SKILL.md` | Behavioral instructions for Claude Code |
| `skill/scripts/install.sh` | Automated installation via `cargo install` |
| `skill/scripts/ensure_indexed.sh` | Ensures `.cartog.db` exists and is up to date |
| `skill/scripts/query.sh` | Thin wrapper running `cartog --json "$@"` |
| `skill/references/query_cookbook.md` | Recipes for common navigation patterns |
| `skill/references/supported_languages.md` | Language support matrix |

## Why It Helps

Instead of Claude Code doing repeated `grep` and `cat` to understand code structure (6+ tool calls, ~2000 tokens), cartog pre-computes a call graph with tree-sitter and stores it in SQLite. Queries return in microseconds (2-3 calls, ~200 tokens, complete picture).

## Supported Languages

Python, TypeScript/JavaScript, Rust, and Go. Java is planned.
