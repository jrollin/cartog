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

At session start, ensure the index exists:

```bash
cartog index .
```

## How It Works

Instead of repeated `grep` and `cat` to understand code structure (6+ tool calls, ~2000 tokens), cartog pre-computes a call graph with tree-sitter and stores it in SQLite. Queries return in microseconds (2-3 calls, ~200 tokens, complete picture).

The skill triggers when the agent needs to navigate code, locate definitions, trace dependencies, assess impact of changes, or support refactoring.

For commands, workflows, and decision heuristics, see [`skills/cartog/SKILL.md`](../skills/cartog/SKILL.md).

## Skill Contents

| File | Purpose |
|------|---------|
| [`SKILL.md`](../skills/cartog/SKILL.md) | Behavioral instructions, commands, and workflows |
| [`scripts/install.sh`](../skills/cartog/scripts/install.sh) | Automated installation via `cargo install` |
| [`scripts/ensure_indexed.sh`](../skills/cartog/scripts/ensure_indexed.sh) | Ensures `.cartog.db` exists and is up to date |
| [`scripts/query.sh`](../skills/cartog/scripts/query.sh) | Thin wrapper running `cartog --json "$@"` |
| [`references/query_cookbook.md`](../skills/cartog/references/query_cookbook.md) | Recipes for common navigation patterns |
| [`references/supported_languages.md`](../skills/cartog/references/supported_languages.md) | Language support matrix |
