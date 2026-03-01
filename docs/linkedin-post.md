# LinkedIn Project Post — cartog

## Short Version (recommended for LinkedIn — ~1,300 characters)

---

I built cartog — an open-source code graph indexer for AI coding agents.

The problem: Every time an AI agent needs to understand your codebase, it runs 6+ grep/cat calls, burns ~1,700 tokens, and still misses 22% of relevant code. Multiply that by every question, every refactor, every debug session.

The fix: Pre-compute the code graph once, query it instantly.

cartog parses your code with tree-sitter, extracts symbols and relationships (calls, imports, inheritance, type references), stores everything in SQLite, and lets your agent query it in 1-2 calls instead of 6+.

Results across 13 benchmarks, 5 languages:
→ 83% fewer tokens per query
→ 97% recall vs 78% with grep
→ 8µs to 17ms query latency
→ 88% token reduction on call chain tracing

What makes it different:
• Single binary — no language server, no Docker, no config
• 100% offline — tree-sitter + SQLite + ONNX embeddings. Your code never leaves your machine
• Dual search — keyword (sub-ms) + semantic (natural language over code)
• Live indexing — file watcher auto re-indexes on changes
• MCP server — plug into Claude Code, Cursor, Windsurf, Zed

Supports Python, TypeScript, JavaScript, Rust, Go, and Ruby. MIT licensed.

Install: cargo install cartog
GitHub: https://github.com/jrollin/cartog

If you're using AI coding agents and tired of watching them grep around blindly — give it a try.

#OpenSource #AI #CodingAgents #Rust #DeveloperTools #LLM #CodeNavigation

---

## Long Version (~2,200 characters)

---

I've been working on something for the past few months and it's time to share it.

cartog is an open-source code graph indexer designed for LLM coding agents — Claude Code, Cursor, Aider, and others.

The problem it solves:

Every time an AI agent needs to understand your codebase, it runs repeated grep and cat commands. For a simple question like "who calls this function?", that's 6+ tool calls, ~1,700 tokens, and still only 78% of relevant code found. Now multiply that by every question across a coding session.

The approach:

Code is a graph — functions call other functions, classes inherit, modules import. cartog pre-computes this graph using tree-sitter parsing, stores it in SQLite, and gives agents instant structural queries:

• `cartog refs validate_token` → who calls this?
• `cartog impact SessionManager --depth 3` → what breaks if I change this?
• `cartog rag search "authentication flow"` → natural language over code

Results measured across 13 scenarios, 5 languages:
→ 83% fewer tokens per query (~280 vs ~1,700)
→ 97% recall vs 78% with grep-based navigation
→ Sub-millisecond keyword search, 8µs outline queries
→ 88% token reduction on call chain tracing — the hardest task for grep

Design principles I'm most proud of:

1. Zero external dependencies — single binary + one SQLite file. No language server, no Docker, no graph database, no config files.

2. 100% offline and private — tree-sitter parsing, SQLite storage, and ONNX-based embeddings all run locally. No API keys. No telemetry. No data leaves your machine. Works in air-gapped environments.

3. Semantic + structural search — keyword search for when you know the name, neural embeddings for when you know the concept. FTS5 + vector KNN + cross-encoder re-ranking, all local.

4. MCP server — `cartog serve` exposes 11 tools over stdio. Works with Claude Code, Cursor, Windsurf, Zed, and any MCP-compatible client.

Currently supports Python, TypeScript/JavaScript, Rust, Go, and Ruby. MIT licensed. Written in Rust.

→ GitHub: https://github.com/jrollin/cartog
→ Install: cargo install cartog
→ Crates.io: https://crates.io/crates/cartog

If you work with AI coding agents and want them to navigate code by structure instead of pattern matching — I'd love your feedback.

#OpenSource #AI #Rust #DeveloperTools #CodingAgents #LLM #CodeNavigation #MCP
