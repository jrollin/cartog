# cartog — Project Structure

## Directory Layout

```
cartog/
├── Cargo.toml
├── AGENTS.md                # Guidelines for AI coding agents
├── src/
│   ├── main.rs              # Entry point, CLI dispatch
│   ├── commands.rs          # Command handlers (outline, refs, impact, etc.)
│   ├── cli.rs               # Clap command definitions
│   ├── db.rs                # SQLite schema, CRUD, query methods
│   ├── indexer.rs           # Orchestrates: walk files → extract → store → resolve
│   ├── mcp.rs               # MCP server (tool handlers, path validation, ServerHandler)
│   ├── languages/
│   │   ├── mod.rs           # Language registry, Extractor trait, shared node_text helper
│   │   ├── python.rs        # Python tree-sitter extractor
│   │   ├── typescript.rs    # TypeScript/TSX extractors
│   │   ├── javascript.rs    # JavaScript extractor
│   │   ├── js_shared.rs     # Shared JS/TS extraction logic
│   │   ├── rust_lang.rs     # Rust extractor
│   │   ├── go.rs            # Go extractor
│   │   └── ruby.rs          # Ruby extractor
│   └── types.rs             # Symbol, Edge, FileInfo structs
├── skills/
│   └── cartog/              # Agent Skill (agentskills.io)
│       ├── SKILL.md         # Behavioral instructions for AI agents
│       ├── scripts/
│       │   ├── install.sh
│       │   ├── ensure_indexed.sh
│       │   └── query.sh
│       └── references/
│           ├── query_cookbook.md
│           └── supported_languages.md
├── benchmarks/
│   ├── run.sh               # Benchmark runner (token efficiency, recall, command count)
│   ├── lib/                 # Shared measurement & comparison helpers
│   ├── fixtures/
│   │   ├── webapp_py/       # Python fixture (15 files: auth, models, routes, utils)
│   │   ├── webapp_rs/       # Rust fixture (12 files: auth, models, routes)
│   │   └── webapp_rb/       # Ruby fixture (10 files: auth, models, routes, utils)
│   ├── ground_truth/        # Expected relationships per fixture (JSON)
│   ├── scenarios/           # 7 scenario scripts (01-07)
│   └── results/             # Benchmark output (gitignored)
├── tests/
│   └── fixtures/
│       └── auth/            # Python fixtures for indexer tests
│           ├── tokens.py
│           └── service.py
└── docs/
    ├── product.md           # Product overview
    ├── tech.md              # Technology decisions
    ├── structure.md         # This file
    ├── usage.md             # CLI commands + MCP server setup per client
    └── claude-code.md       # Claude Code integration details
```

## Module Responsibilities

- **cli.rs**: Defines all subcommands via clap derive. No business logic.
- **db.rs**: Owns the SQLite connection. Schema creation, inserts, and all query methods. Returns domain types.
- **indexer.rs**: Walks the file tree, delegates to language extractors, writes to db, runs edge resolution.
- **commands.rs**: All 9 command handlers. Formats output (human-readable or `--json`).
- **mcp.rs**: MCP server over stdio. `CartogServer` struct with 9 `#[tool]` handlers wrapping the same DB methods as `commands.rs`. Path validation restricts `index` to CWD subtree. Uses `spawn_blocking` for sync DB/indexer calls.
- **languages/mod.rs**: Maps file extensions to extractors, defines the `Extractor` trait and shared `node_text` helper. Each extractor implements `fn extract(&self, source: &str, file_path: &str) -> Result<ExtractionResult>`.
- **types.rs**: Shared data structures. No logic beyond Display/serialization.

## Conventions

- No `unwrap()` in library code — use `anyhow::Result` for error propagation.
- All public functions documented with `///` doc comments.
- CLI output: human-readable by default, `--json` for structured output.
- Tests: unit tests in each module (`#[cfg(test)]`), fixture files in `tests/fixtures/` for indexer tests.
