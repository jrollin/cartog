---
name: cartog
description: >-
  Code graph navigation and impact analysis. Use when the user asks
  "where is X defined?", "what calls X?", "who imports X?", "what depends on X?",
  "what breaks if I change X?", "help me refactor X", "show me the call graph",
  "find all usages of X", "show file structure", or needs to navigate code,
  locate definitions, trace dependencies, assess blast radius of changes,
  support refactoring (rename, extract, move), or explore an unfamiliar codebase.
  Supports Python, TypeScript/JavaScript, Rust, Go, Ruby.
---

# cartog — Code Graph Navigation Skill

## When to Use

Use cartog **before** reaching for grep, cat, or file reads when you need to:
- Discover symbols by partial name → `cartog search <query>`
- Find code by concept or behavior → `cartog rag search "description"`
- Understand the structure of a file → `cartog outline <file>`
- Find who references a symbol → `cartog refs <name>` (or `--kind calls` for just callers)
- See what a function calls → `cartog callees <name>`
- Assess refactoring impact → `cartog impact <name> --depth 3`
- Understand class hierarchies → `cartog hierarchy <class>`
- See file dependencies → `cartog deps <file>`

## Why cartog Over grep/glob

cartog pre-computes a code graph (symbols + edges) with tree-sitter and stores it in SQLite. Compared to grep/glob:
- **Fewer tool calls**: 1 command vs 3-6 grep/read cycles
- **Transitive analysis**: `impact --depth 3` traces callers-of-callers — grep can't do this
- **Structured results**: symbols with types, signatures, and line ranges — not raw text matches

## Workflow Rules

1. **Before you grep or read a file to understand structure**, query cartog first.

2. **Search routing** — pick the right strategy based on the query:

   **A. Direct keyword search** (`cartog search <name>`) — when the query is clearly a symbol identifier: single token, camelCase/snake_case, partial name. Sub-millisecond, prefix + substring matching.
   ```
   cartog search validate_token
   cartog search AuthService
   cartog search parse --kind function
   ```

   **B. Semantic search** (`cartog rag search "<query>"`) — when the query is natural language describing behavior or a concept, and you don't know the symbol name.
   ```
   cartog rag search "authentication token validation"
   cartog rag search "database connection pooling"
   cartog rag search "error handling in HTTP requests"
   ```

   **C. Both in parallel** — when the query is a broad keyword that could be a symbol name AND a concept (e.g., `auth`, `config`, `cache`). Run both in parallel to combine exact name matches with semantically related code.
   ```
   # run in parallel
   cartog search auth
   cartog rag search "authentication and authorization"
   ```

   **Narrowing pattern**: when `cartog search` returns too many results, use `rag search` with a more descriptive query to narrow down. Example: `search parse` returns 30 hits → `rag search "parse JSON response body"` pinpoints the right ones.

   **Routing rules**:
   - Single identifier-like token → **A** (direct)
   - Multi-word natural language → **B** (semantic)
   - Broad keyword / unsure → **C** (parallel)
   - Too many results from A → narrow with **B**

3. **When using `cartog search`** to locate a symbol before `refs`/`callees`/`impact`:
   - Exactly one result → use that symbol name and file, proceed.
   - Multiple results, same name, different files → add `--file <path>` to disambiguate.
   - Multiple results, different names → add `--kind <kind>` to filter, then re-evaluate.
   - Never pass an ambiguous name to `refs`/`callees`/`impact` — the result will be wrong.

4. **Use `cartog outline <file>`** instead of `cat <file>` when you need structure, not content.

5. **Before refactoring**, run `cartog impact <symbol>` to see the blast radius.

6. **Only fall back to grep/read** when cartog doesn't have what you need (e.g., reading actual implementation logic, string literals, config values).

7. **After making code changes**, run `cartog index .` to update the graph.

## Setup

Before first use, ensure cartog is installed and indexed:

```bash
# Install if missing
command -v cartog || bash scripts/install.sh

# Index (incremental — safe to re-run)
cartog index .

# Enable semantic search (required for rag search routing)
cartog rag setup          # download embedding + re-ranker models (one-time)
cartog rag index .        # embed all symbols for vector search
```

Without `rag setup`, the `rag search` strategy is unavailable and the agent should use `search` + grep as fallback.

## Commands Reference

### Index (build/rebuild)
```bash
cartog index .                    # Index current directory
cartog index src/                 # Index specific directory
cartog index . --force            # Re-index all files (ignore cache)
```

### Search (find symbols by partial name)
```bash
cartog search parse                          # prefix + substring match
cartog search parse --kind function          # filter by symbol kind
cartog search config --file src/db.rs        # filter to one file
cartog search parse --limit 10               # cap results
```
Returns symbols ranked: exact match → prefix → substring. Case-insensitive. Max 100 results.

Valid `--kind` values: `function`, `class`, `method`, `variable`, `import`.

### RAG Semantic Search (natural language queries)
```bash
cartog rag search "authentication token validation"
cartog rag search "error handling" --kind function
cartog rag search "database schema setup" --limit 20
```

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
Available `--kind` values: `calls`, `imports`, `inherits`, `references`, `raises`.

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

### Serve (MCP server)
```bash
cartog serve                    # MCP server over stdio
cartog serve --watch            # with background file watcher
cartog serve --watch --rag      # watcher + deferred RAG embedding
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

1. **Identify** — `cartog search <name>` to confirm the exact symbol name and file
2. **Map references** — `cartog refs <name>` to find every usage
3. **Assess blast radius** — `cartog impact <name> --depth 3` for transitive dependents
4. **Check hierarchy** — `cartog hierarchy <name>` if it's a class (subclasses need updating too)
5. **Plan change order** — update leaf dependents first, work inward toward the source
6. **Apply changes** — modify files
7. **Re-index** — `cartog index .` to update the graph
8. **Verify** — re-run `cartog refs <name>` to confirm no stale references remain

## Decision Heuristics

| I need to... | Use |
|---|---|
| Find a symbol by exact/partial name | `cartog search <name>` |
| Find code by concept or behavior | `cartog rag search "description"` |
| Broad keyword, unsure which to use | Both `search` + `rag search` in parallel |
| Too many results from `search` | Narrow with `rag search "more specific description"` |
| Know what's in a file | `cartog outline <file>` |
| Find usages of a function | `cartog refs <name>` (`--kind calls` for just callers) |
| See what a function calls | `cartog callees <name>` |
| Check if a change is safe | `cartog impact <name> --depth 3` |
| Understand class hierarchy | `cartog hierarchy <class>` |
| See file dependencies | `cartog deps <file>` |
| Read actual implementation logic | `cat <file>` (cartog indexes structure, not content) |
| Search for string literals / config | `grep` (cartog doesn't index these) |
| Nothing from search or rag | Fall back to `grep` |

## Limitations

- Structural/heuristic resolution, not full semantic. ~90% accuracy for cross-file references.
- Currently supports: Python, TypeScript/JavaScript, Rust, Go, Ruby. Java planned.
- Does not index string literals, comments (except docstrings), or config values.
- Method resolution is name-based — `foo.bar()` resolves `bar`, not `Foo.bar` specifically.

### RAG search limitations

- **No substring matching**: `"valid"` does NOT match `validate_token`. FTS5 is token-based. Use `cartog search` for substring matches (this is why parallel routing is valuable).
- **Requires setup**: if `cartog rag setup` was not run, `rag search` is unavailable.
- **Scores are relative**: `rrf_score` and `rerank_score` values are only meaningful for ranking within a single query — don't compare scores across different queries.
- **Re-ranking latency**: cross-encoder scores all candidates in a single batch ONNX call (up to 50 candidates). Expect ~150-500ms total overhead depending on candidate count.
