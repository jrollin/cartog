---
name: cartog
description: >-
  Code graph navigation, semantic code search, and impact analysis. Use when the user asks
  "where is X defined?", "what calls X?", "who imports X?", "what depends on X?",
  "what does X call?", "how is X used?", "where is X called from?",
  "what breaks if I change X?", "is it safe to change/delete X?",
  "help me refactor X", "show me the call graph", "trace the execution flow",
  "find all usages of X", "find code related to...", "find the function/class that handles...",
  "show file structure", "list all functions/classes in a file",
  "show the inheritance tree", "show class hierarchy",
  or needs to navigate code, locate definitions, search code by concept or behavior,
  trace dependencies, assess blast radius of changes, explore how a feature is implemented,
  support refactoring (rename, extract, move, delete), or explore an unfamiliar codebase.
  Supports Python, TypeScript/JavaScript, Rust, Go, Ruby.
---

# cartog — Code Graph Navigation Skill

## When to Use

Use cartog **before** reaching for grep, cat, or file reads when you need to:
- Find code by name, concept, or behavior → `cartog rag search "query"`
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

   **A. Semantic search** (`cartog rag search "<query>"`) — **default for all searches**. Handles keyword matching (FTS5), vector similarity, and cross-encoder reranking in a single call. Works for both natural language and keyword-style queries. Always use ONE call with the full query — never split a query into multiple rag search calls.
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

3. **When using `cartog search`** to locate a symbol before `refs`/`callees`/`impact`:
   - Exactly one result → use that symbol name and file, proceed.
   - Multiple results, same name, different files → add `--file <path>` to disambiguate.
   - Multiple results, different names → add `--kind <kind>` to filter, then re-evaluate.
   - Never pass an ambiguous name to `refs`/`callees`/`impact` — the result will be wrong.

4. **Use `cartog outline <file>`** instead of `cat <file>` when you need structure, not content.

5. **Before refactoring**, run `cartog impact <symbol>` to see the blast radius.

6. **Only fall back to grep/read** when cartog doesn't have what you need (e.g., reading actual implementation logic, string literals, config values).

7. **After making code changes**, run `cartog index .` to update the graph.

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

## Setup

Before first use, ensure cartog is installed and indexed:

```bash
# Install if missing
command -v cartog || bash scripts/install.sh

# Run the setup script (handles all 3 phases)
bash scripts/ensure_indexed.sh
```

### Search quality tiers

`cartog rag search` works at three quality levels depending on setup state:

| Tier | After | FTS5 | Reranker | Vector | Quality |
|---|---|---|---|---|---|
| 1 | `cartog index .` | Yes | No | No | Keyword matching only |
| 2 | `+ cartog rag setup` | Yes | **Yes** | No | Keyword + neural reranking |
| 3 | `+ cartog rag index .` | Yes | Yes | **Yes** | Full hybrid (best) |

The setup script runs tier 1+2 blocking (fast), then tier 3 in the background.
`cartog rag search` is usable immediately after tier 2 — vector search becomes available
transparently once background embedding completes.

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

### RAG Search (hybrid keyword + semantic)
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
| Find code by name, concept, or behavior | `cartog rag search "query"` |
| Get a symbol name for structural commands | `cartog search <name>` |
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

- **No substring matching**: `"valid"` does NOT match `validate_token`. FTS5 is token-based. If `rag search` returns no results for a known symbol name, fall back to `cartog search` which supports substring matching.
- **Graceful degradation**: `rag search` works without `rag setup` or `rag index` (FTS5-only). Quality improves with each setup tier (see Search quality tiers above).
- **Scores are relative**: `rrf_score` and `rerank_score` values are only meaningful for ranking within a single query — don't compare scores across different queries.
- **Re-ranking latency**: cross-encoder scores all candidates in a single batch ONNX call (up to 50 candidates). Expect ~150-500ms total overhead depending on candidate count.
