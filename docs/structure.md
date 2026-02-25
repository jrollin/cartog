# cartog — Project Structure

## Directory Layout

```
cartog/
├── Cargo.toml
├── AGENTS.md                # Guidelines for AI coding agents
├── src/
│   ├── main.rs              # Entry point, CLI dispatch
│   ├── commands.rs          # Command handlers (outline, callers, impact, etc.)
│   ├── cli.rs               # Clap command definitions
│   ├── db.rs                # SQLite schema, CRUD, query methods
│   ├── indexer.rs           # Orchestrates: walk files → extract → store → resolve
│   ├── languages/
│   │   ├── mod.rs           # Language registry, Extractor trait, shared node_text helper
│   │   ├── python.rs        # Python tree-sitter extractor
│   │   ├── typescript.rs    # TypeScript/TSX extractors
│   │   ├── javascript.rs    # JavaScript extractor
│   │   ├── js_shared.rs     # Shared JS/TS extraction logic
│   │   ├── rust_lang.rs     # Rust extractor
│   │   └── go.rs            # Go extractor
│   └── types.rs             # Symbol, Edge, FileInfo structs
├── skill/
│   ├── SKILL.md             # Claude behavioral instructions
│   ├── scripts/
│   │   ├── install.sh
│   │   ├── ensure_indexed.sh
│   │   └── query.sh
│   └── references/
│       ├── query_cookbook.md
│       └── supported_languages.md
└── docs/
    ├── product.md           # Product overview
    ├── tech.md              # Technology decisions
    └── structure.md         # This file
```

## Module Responsibilities

- **cli.rs**: Defines all subcommands via clap derive. No business logic.
- **db.rs**: Owns the SQLite connection. Schema creation, inserts, and all query methods. Returns domain types.
- **indexer.rs**: Walks the file tree, delegates to language extractors, writes to db, runs edge resolution.
- **commands.rs**: All 9 command handlers. Formats output (human-readable or `--json`).
- **languages/mod.rs**: Maps file extensions to extractors, defines the `Extractor` trait and shared `node_text` helper. Each extractor implements `fn extract(&self, source: &str, file_path: &str) -> Result<ExtractionResult>`.
- **types.rs**: Shared data structures. No logic beyond Display/serialization.

## Conventions

- No `unwrap()` in library code — use `anyhow::Result` for error propagation.
- All public functions documented with `///` doc comments.
- CLI output: human-readable by default, `--json` for structured output.
- Tests: unit tests in each module, integration tests in `tests/` against real Python files.
