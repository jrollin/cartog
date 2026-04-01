---
name: cartog
description: >-
  Code graph navigation, semantic code search, and impact analysis. Use when the user asks
  "where is X defined?", "what calls X?", "who imports X?", "what depends on X?",
  "how is X used?", "where is X called from?",
  "what breaks if I change X?", "is it safe to change/delete X?",
  "help me refactor X", "show me the call graph", "trace the execution flow",
  "find all usages of X", "find code related to...",
  "show me how X works", "show me the X implementation",
  "show file structure", "list all functions/classes in a file",
  "show the inheritance tree", "show class hierarchy",
  or needs to navigate code, locate definitions, search code by concept or behavior,
  trace dependencies, assess blast radius of changes, explore how a feature is implemented,
  support refactoring (rename, extract, move, delete), or explore an unfamiliar codebase.
  Supports Python, TypeScript/JavaScript, Rust, Go, Ruby, Java, and Markdown documents.
---

# cartog — Code Graph Navigation Skill

## When to Use

Use cartog **before** reaching for grep, cat, or file reads when you need to:
- Find code by name, concept, or behavior → `cartog rag search "query"`
- Search project documentation → `cartog rag search "query" --kind document`
- Understand the structure of a file → `cartog outline <file>`
- Find who references a symbol → `cartog refs <name>` (or `--kind calls` for just callers)
- See what a function calls → `cartog callees <name>`
- Assess refactoring impact → `cartog impact <name> --depth 3`
- Understand class hierarchies → `cartog hierarchy <class>`
- See file dependencies → `cartog deps <file>`
- Get a codebase overview → `cartog map [--tokens N]`
- See what changed recently → `cartog changes [--commits N]`

## How to Run

cartog has two modes — **CLI** and **MCP server**. Use whichever is available:

| Check | Mode | How to invoke |
|---|---|---|
| MCP tools named `cartog_*` exist in your tool list | **MCP** | Call the MCP tools directly |
| No `cartog_*` MCP tools available | **CLI** | Run commands via the **Bash tool** |

**Do not** fall back to grep/glob if MCP tools are missing — use the Bash tool instead.

**Rules for both modes:**
- Run independent `cartog` commands as **parallel tool calls** — whether MCP or Bash
- **Subagents**: MCP tools are only available to the main agent. Subagents should always use CLI mode via Bash. Skip setup — assume the parent has already indexed

**CLI mode:** every `cartog` command in the examples below must be run via a **Bash tool call**. Do not chain with `&&` or `|` — use separate Bash calls.

All examples below use CLI syntax. MCP tool names and parameters:

| CLI command | MCP tool | Parameters |
|---|---|---|
| `cartog index .` | `cartog_index` | `path`, `force` |
| `cartog search <name>` | `cartog_search` | `query`, `kind?`, `file?`, `limit?` |
| `cartog rag search "<query>"` | `cartog_rag_search` | `query`, `kind?`, `limit?` |
| `cartog rag index .` | `cartog_rag_index` | `path`, `force` |
| `cartog outline <file>` | `cartog_outline` | `file` |
| `cartog refs <name>` | `cartog_refs` | `name`, `kind?` |
| `cartog callees <name>` | `cartog_callees` | `name` |
| `cartog impact <name>` | `cartog_impact` | `name`, `depth?` |
| `cartog hierarchy <class>` | `cartog_hierarchy` | `name` |
| `cartog deps <file>` | `cartog_deps` | `file` |
| `cartog changes` | `cartog_changes` | `commits?`, `kind?` |
| `cartog stats` | `cartog_stats` | — |

## Why cartog Over grep/glob

cartog pre-computes a code graph (symbols + edges) with tree-sitter and stores it in SQLite. Compared to grep/glob:
- **Fewer tool calls**: 1 command vs 3-6 grep/read cycles
- **Transitive analysis**: `impact --depth 3` traces callers-of-callers — grep can't do this
- **Structured results**: symbols with types, signatures, and line ranges — not raw text matches

## Workflow Rules

1. **Before you grep or read a file to understand structure**, query cartog first.

2. **Search routing** — pick the right strategy based on the query:

   **A. Semantic search** (`cartog rag search "<query>"`) — **default for all searches**. Returns code only by default; use `--kind document` for docs or `--kind all` for both. Handles keyword matching (FTS5), vector similarity, and cross-encoder reranking in a single call. Works for both natural language and keyword-style queries. Always use ONE call with the full query — never split a query into multiple rag search calls.
   ```
   cartog rag search "authentication token validation"
   cartog rag search "contract management and timesheet signing"
   cartog rag search "config"
   ```

   **B. Structural search** (`cartog search <name>`) — use **only** when you need a symbol name to feed into `refs`, `callees`, `impact`, or `hierarchy`. These commands require exact symbol names, not search results.
   ```
   cartog search validate_token
   cartog search AuthService --kind class
   ```

   **Routing rules**:
   - Need to find code? → **A** (rag search) — always
   - Need a symbol name for `refs`/`callees`/`impact`? → **B** (search) first, then the structural command
   - User already gave an exact symbol name? → call `refs`/`callees`/`impact` directly — skip `search`

3. **When using `cartog search`** to locate a symbol before `refs`/`callees`/`impact`:
   - Exactly one result → use that symbol name and file, proceed.
   - Multiple results, same name, different files → add `--file <path>` to disambiguate.
   - Multiple results, different names → add `--kind <kind>` to filter, then re-evaluate.
   - Never pass an ambiguous name to `refs`/`callees`/`impact` — the result will be wrong.

4. **Use `cartog outline <file>`** instead of `cat <file>` when you need structure, not content. Then use `Read` (with offset/limit) for the specific lines you need — this is more efficient than reading entire files.

5. **Before refactoring**, run `cartog impact <symbol>` to see the blast radius.

6. **Only fall back to grep/read** when cartog doesn't have what you need (e.g., reading actual implementation logic, string literals, config values).

7. **After making code changes**, run `cartog index . --no-lsp` to quickly update the graph.

## Do / Don't

**DO:**
- Use `cartog rag search` as your default search — it combines FTS5 keyword + vector + reranking in one call
- Use `cartog search` only to get a symbol name for structural commands (`refs`, `callees`, `impact`, `hierarchy`)
- Trust that `rag search` degrades gracefully — FTS5 works even without vector embeddings

**DON'T:**
- Run `cartog search` and `cartog rag search` in parallel for the same query — this wastes a tool call. `rag search` already includes FTS5 keyword matching internally
- Split one query into multiple `rag search` calls with rephrased variants — one call is enough. The hybrid search (FTS5 + vector + reranker) handles synonyms and related terms internally
- Block on RAG embedding at setup — background indexing is fine, `rag search` works immediately with FTS5 + reranker
- Assume `rag search` requires `rag index` — it works (at reduced quality) with just `cartog index .`
- Chain multiple `cartog` CLI commands with `&&` or `|` — each invocation opens a fresh SQLite connection with full initialization overhead (PRAGMAs, schema checks, cold cache). Run them as **separate tool calls** instead
- Pipe `cartog` output through `grep` — cartog already returns focused, structured results. Filtering with grep discards context (line numbers, kinds, file paths) and can break `&&` chains when grep finds no match (exit code 1)
- Combine unrelated cartog queries in one bash command — this creates false dependencies and hides failures. See `references/query_cookbook.md` → "Anti-patterns to avoid" for examples

## Setup

Before first use, ensure cartog is installed and indexed.

If the project uses Ollama (check `.cartog.toml` for `[embedding] provider = "ollama"`), skip `rag setup` — models are managed by the Ollama server.

The `scripts/` directory is located next to this SKILL.md file. **Before running any setup command**, look at the absolute path from which this SKILL.md was loaded (visible in your tool call history), take its parent directory, and use that as the scripts root in the bash commands below.

For example: if this file was loaded from `/home/user/.claude/skills/cartog/SKILL.md`, run:
```bash
# Install if missing
command -v cartog || bash "/home/user/.claude/skills/cartog/scripts/install.sh"

# Run the setup script (handles version check + 3 indexing phases)
bash "/home/user/.claude/skills/cartog/scripts/ensure_indexed.sh"
```

The setup script checks for newer cartog versions (cached, at most once per 24h).
If an update is available it prints a notice like:
```
New cartog version available: 0.7.0 (installed: 0.6.1). Update with: bash "/path/to/skill/scripts/install.sh" 0.7.0
```
When you see this notice, ask the user if they want to update before continuing. If they agree, run the suggested command, then re-run `bash "/path/to/skill/scripts/ensure_indexed.sh"`.

### Search quality tiers

`cartog rag search` works at three quality levels depending on setup state:

| Tier | After | FTS5 | Reranker | Vector | Quality |
|---|---|---|---|---|---|
| 1 | `cartog index .` | Yes | No | No | Keyword matching only |
| 2 | `+ cartog rag setup` | Yes | **Yes** | No | Keyword + neural reranking |
| 3 | `+ cartog rag index .` | Yes | Yes | **Yes** | Full hybrid (best) |

The setup script runs tier 1+2 blocking, then tier 3 in the background.
`cartog rag search` is usable immediately after tier 2 — vector search becomes available
transparently once background embedding completes.

> **First run**: tier 2 downloads ~1.2GB of ONNX models (cached in `~/.cache/cartog/models/`).
> This may take a few minutes — do not abort. Subsequent runs are instant.

## Database Location

The index is stored in a SQLite database. cartog resolves the path automatically:

| Priority | Source |
|----------|--------|
| 1 | `--db <path>` flag or `CARTOG_DB` env var |
| 2 | `.cartog.toml` → `[database] path = "..."` at git root |
| 3 | Auto git-root: DB placed at the root of the current git repository |
| 4 | `.cartog.db` in the current directory (fallback) |

For most projects, no configuration is needed — running `cartog index .` from any subdirectory will place the DB at the git root automatically.

```bash
# Override examples
cartog --db /tmp/myproject.db index .
CARTOG_DB=~/.local/share/cartog/proj.db cartog index .
```

## Commands Reference

### Index (build/rebuild)
```bash
cartog index .                    # Index current directory (with LSP if available)
cartog index . --no-lsp           # Fast heuristic-only index (~1-4s)
cartog index src/                 # Index specific directory
cartog index . --force            # Re-index all files (ignore cache)
```

When compiled with `--features lsp`, `cartog index .` auto-detects language servers on PATH and uses them to resolve additional edges. LSP results are **persisted in the database** — subsequent queries benefit without re-running LSP. Use `--no-lsp` for fast day-to-day indexing.

### Search (find symbols by partial name)
```bash
cartog search parse                          # prefix + substring match
cartog search parse --kind function          # filter by symbol kind
cartog search config --file src/db.rs        # filter to one file
cartog search parse --limit 10               # cap results
```
Returns symbols ranked: exact match → prefix → substring. Case-insensitive. Max 100 results.

Valid `--kind` values: `function`, `class`, `method`, `variable`, `import`, `interface`, `enum`, `type-alias`, `trait`, `module`, `document`.

### RAG Search (hybrid keyword + semantic)
```bash
cartog rag search "authentication token validation"
cartog rag search "error handling" --kind function
cartog rag search "database schema setup" --limit 20
cartog rag search "deployment architecture" --kind document
```

By default, returns code only. Use `--kind document` for docs or `--kind all` for both.
Uses hybrid retrieval: FTS5 keyword matching + vector KNN, merged via Reciprocal Rank Fusion.
When the cross-encoder model is available, results are re-ranked for better precision.

### Outline (file structure)
```bash
cartog outline src/auth/tokens.py
```
Output shows symbols with types, signatures, and line ranges — no need to read the file.

### Refs (who references this?)
```bash
cartog refs validate_token               # all reference types
cartog refs validate_token --kind calls  # only call sites
```
Available `--kind` values: `calls`, `imports`, `inherits`, `references`, `raises`, `implements`, `type-of`.

### Callees (what does this call?)
```bash
cartog callees authenticate
```

### Impact (transitive blast radius)
```bash
cartog impact SessionManager --depth 3
```
Shows everything that transitively depends on a symbol up to N hops.

### Hierarchy (inheritance tree)
```bash
cartog hierarchy BaseService
```

### Deps (file imports)
```bash
cartog deps src/routes/auth.py
```

### Map (codebase overview)
```bash
cartog map                               # default 4000 tokens
cartog map --tokens 2000                 # compact
cartog map --tokens 8000                 # detailed
```
File tree + top symbols ranked by reference count (centrality). Use at the start of a session for context loading.

### Changes (recently modified symbols)
```bash
cartog changes                           # last 5 commits + working tree
cartog changes --commits 10              # last 10 commits
cartog changes --kind function           # only functions
```
Shows symbols affected by recent git changes, grouped by file.

### Stats (index summary)
```bash
cartog stats
```

### Watch (auto re-index on file changes)
```bash
cartog watch .                           # watch current directory
cartog watch . --rag                     # also re-embed symbols (deferred)
cartog watch . --debounce 3 --rag-delay 30  # custom timings
```

Watch always uses heuristic-only indexing (no LSP) for speed. Previously LSP-resolved edges are preserved in the DB.

### Serve (MCP server)
```bash
cartog serve                    # MCP server over stdio
cartog serve --watch            # with background file watcher
cartog serve --watch --rag      # watcher + deferred RAG embedding
```

When an agent calls `cartog_index` via MCP, LSP servers are started once and **kept warm** for the session. Subsequent index calls reuse warm servers (~2s instead of 60s). Background watch re-indexing stays heuristic-only.

## Token Budget

Use `--tokens N` to limit output to approximately N tokens (human-readable only, ignored with `--json`):
```bash
cartog --tokens 500 search validate
cartog --tokens 200 outline src/db.rs
cartog --tokens 1000 changes --commits 10
```

## JSON Output

All commands support `--json` for structured output:
```bash
cartog --json refs validate_token
cartog --json outline src/auth/tokens.py
cartog --json rag search "authentication"
```

## Refactoring Workflow

Before changing any symbol (rename, extract, move, delete):

### Phase 1: Narrow with heuristic graph (~1s)

1. **Identify** — `cartog search <name>` to confirm the exact symbol name and file
2. **Map references** — `cartog refs <name>` to find every usage
3. **Assess blast radius** — `cartog impact <name> --depth 3` for transitive dependents
4. **Check hierarchy** — `cartog hierarchy <name>` if it's a class (subclasses need updating too)

### Phase 2: Upgrade to LSP if needed (~15-60s first call)

Upgrade to LSP when the heuristic graph has gaps:
- `refs` returns **fewer results than expected** (you know more callers exist)
- **Ambiguous symbol** — two classes have the same method name, `impact` can't tell which is called
- `--json` output shows `target_id: null` on edges you care about

```bash
cartog index .                   # re-index with LSP auto-detected (if compiled with lsp feature)
cartog impact <name> --depth 3   # re-check — more edges resolved
```

If no LSP servers are on PATH, indexing silently falls back to heuristic-only.
Use `--no-lsp` to skip LSP when you want speed over precision.

> **Performance tip**: each `cartog index .` via Bash spawns LSP servers from scratch (~15-60s).
> If you plan multiple index calls in a session (refactoring loop), use `cartog serve` as an MCP server instead — it keeps LSP servers warm across calls, making the 2nd+ index near-instant.

### Phase 3: Apply and verify

5. **Plan change order** — update leaf dependents first, work inward toward the source
6. **Apply changes** — modify files
7. **Re-index** — `cartog index . --no-lsp` to quickly update the graph
8. **Verify** — re-run `cartog refs <name>` to confirm no stale references remain

## Decision Heuristics

| I need to... | Use |
|---|---|
| Find code by name, concept, or behavior | `cartog rag search "query"` |
| Search project documentation | `cartog rag search "query" --kind document` |
| Search both code and docs | `cartog rag search "query" --kind all` |
| Get a symbol name for structural commands | `cartog search <name>` |
| Know what's in a file | `cartog outline <file>` |
| Find usages of a function | `cartog refs <name>` (`--kind calls` for just callers) |
| See what a function calls | `cartog callees <name>` |
| Check if a change is safe | `cartog impact <name> --depth 3` |
| Understand class hierarchy | `cartog hierarchy <class>` |
| See file dependencies | `cartog deps <file>` |
| Get a codebase overview | `cartog map` (`--tokens N` for budget control) |
| See what changed recently | `cartog changes` (`--commits N` for more history) |
| Improve graph precision for a refactoring | `cartog index .` (with LSP auto-detected) |
| Fast re-index after code changes | `cartog index . --no-lsp` |
| Read actual implementation logic | `cat <file>` (cartog indexes structure, not content) |
| Search for string literals / config | `grep` (cartog doesn't index these) |
| Nothing from search or rag | Fall back to `grep` |

## Limitations

- Heuristic resolution is name-based (~25% of edges resolved). With LSP enabled, ~42-81% resolved depending on language. Remaining unresolved edges are mostly calls to external libraries.
- Code languages: Python, TypeScript/JavaScript, Rust, Go, Ruby, Java.
- Documents: Markdown (`.md`) — indexed by heading sections for semantic search.
- Does not index string literals, comments (except docstrings), or config values.
- Method resolution is name-based without LSP — `foo.bar()` resolves `bar`, not `Foo.bar` specifically. LSP resolves to the exact type when a language server is available.

### LSP limitations

- **Opt-in feature**: requires `cargo install cartog --features lsp`. Without the feature, `--no-lsp` is implied.
- **Auto-detected**: if language servers are on PATH, they are used automatically during `cartog index`. Use `--no-lsp` to skip.
- **Startup latency**: language servers take 15-60s to load a project on first call. Day-to-day indexing should use `--no-lsp`.
- **CLI vs MCP**: each `cartog index .` via Bash spawns and kills LSP servers (cold start). Use `cartog serve` (MCP mode) for sessions with multiple index calls — it keeps servers warm across tool calls.
- **Supported servers**: rust-analyzer, pyright-langserver, typescript-language-server, gopls, ruby-lsp, solargraph, jdtls. Install hints shown when servers are missing.
- **External crate edges stay unresolved**: LSP resolves definitions within the project. Calls to std/external crates remain unresolved regardless.

### RAG search limitations

- **No substring matching**: `"valid"` does NOT match `validate_token`. FTS5 is token-based. If `rag search` returns no results for a known symbol name, fall back to `cartog search` which supports substring matching.
- **Graceful degradation**: `rag search` works without `rag setup` or `rag index` (FTS5-only). Quality improves with each setup tier (see Search quality tiers above).
- **Scores are relative**: `rrf_score` and `rerank_score` values are only meaningful for ranking within a single query — don't compare scores across different queries.
- **Re-ranking latency**: cross-encoder scores all candidates in a single batch ONNX call (up to 50 candidates). Expect ~150-500ms total overhead depending on candidate count.
- **Auto re-embed**: when cartog upgrades its embedding format (e.g., AST-aware chunking), `cartog rag index` automatically detects the change and re-embeds all symbols. No `--force` needed.
