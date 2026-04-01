# cartog Query Cookbook

## Common Patterns

### "Give me an overview of this codebase"
```bash
cartog map                           # file tree + top symbols (4000 tokens)
cartog map --tokens 8000             # more detail
```

### "Where is this symbol defined?"

Start broad, then narrow only if needed:
```bash
cartog search extract
# → 1 result? Use it. Done.
# → many results with different names? Add --kind:
cartog search extract --kind method
# → still multiple files? Add --file:
cartog search extract --kind method --file src/languages/python.rs
# → now unique: proceed with outline/refs/callees
```

### "What does this file contain?"
```bash
cartog outline src/auth/tokens.py
```

### "Who calls this function?"
```bash
cartog refs validate_token --kind calls
```

### "What does this function depend on?"
```bash
cartog callees authenticate
```

### "Is it safe to change this?"
```bash
cartog impact validate_token --depth 3
```

### "Show me the class hierarchy"
```bash
cartog hierarchy BaseService
```

### "What does this file import?"
```bash
cartog deps src/routes/auth.py
```

### "Find everything related to a name"
```bash
cartog refs SessionManager
```

### "What code changed recently?"
```bash
cartog changes                           # last 5 commits + working tree
cartog changes --commits 10              # last 10 commits
cartog changes --kind function           # only functions that changed
```

## Combining Commands

Run each cartog command as a **separate tool call** — don't chain with `&&` or pipe through `grep`. Each CLI invocation opens a fresh SQLite connection with full initialization overhead. Separate calls let the agent inspect each result before deciding the next step.

### Understand a function before modifying it
```bash
cartog search validate_token           # Locate the symbol — note the file path returned
cartog outline <file-from-search>      # See structure of that file
cartog callees validate_token          # What it depends on
cartog impact validate_token           # Who depends on it
```

### Trace a call chain
```bash
cartog callees handle_request          # What does it call?
cartog callees authenticate            # What does authenticate call?
cartog callees validate_token          # Keep going deeper
```

### Assess refactoring scope (narrow first, LSP if needed)

**Step 1 — Fast heuristic pass (~1s):**
```bash
cartog search OldClassName             # Confirm exact name and file first
cartog refs OldClassName               # All references
cartog hierarchy OldClassName          # Subclasses to update
cartog impact OldClassName --depth 5   # Full blast radius
```

If results look complete → proceed with refactoring.

**Step 2 — Upgrade to LSP if gaps found (~15-60s):**

Signs you need LSP:
- `refs` shows fewer callers than expected
- Two classes share the same method name and `impact` can't disambiguate
- `--json` output has `target_id: null` on edges you care about

```bash
cartog index .                         # Re-index with LSP (auto-detected if on PATH)
cartog impact OldClassName --depth 5   # Re-check with higher-precision graph
```

**Step 3 — After refactoring:**
```bash
cartog index . --no-lsp                # Fast re-index to update the graph
cartog refs OldClassName               # Verify no stale references remain
```

### Anti-patterns to avoid

Don't chain cartog calls in a single bash command:
```bash
# BAD — chained with && and grep filtering
cartog callees getLogin 2>&1 | grep "Manager" && cartog callees ssoCallback 2>&1

# BAD — grep discards structured output, && creates false dependency
cartog refs validate_token --kind calls 2>&1 | grep "auth" && cartog impact validate_token

# GOOD — separate calls, inspect each result
cartog callees getLogin
cartog callees ssoCallback
```

## Semantic Search (RAG)

### Setup (one-time)
```bash
cartog rag setup          # download embedding + re-ranker models
cartog rag index .        # embed all symbols + documents
```

#### Ollama provider

If the project uses Ollama for embeddings (configured in `.cartog.toml`):

```bash
# No rag setup needed — models are managed by Ollama
ollama pull nomic-embed-text        # ensure model is available
cartog rag index .                  # embed with Ollama
cartog rag search "error handling"  # search works the same
```

#### Troubleshooting

- **"Unknown or disabled embedding provider: 'ollama'"** — Install with `cargo install cartog --features ollama-embedding`.
- **"Failed to connect to Ollama server"** — Ensure Ollama is running (`ollama serve`).
- **"Embedding dimension changed"** — Provider switch detected. Run `cartog rag index` to re-embed.

### "Find code related to a concept"
```bash
cartog rag search "parse abstract syntax tree"
cartog rag search "handle HTTP authentication" --kind function
cartog rag search "database migration" --limit 5
```

### "Search project documentation"
```bash
cartog rag search "deployment architecture" --kind document
cartog rag search "API rate limiting" --kind document
cartog rag search "error handling strategy" --kind document
```

Markdown files (`.md`) are indexed alongside code — each heading section becomes a searchable document. By default, `rag search` returns code only. Use `--kind document` for docs only, or `--kind all` for both code and docs together.

### After code changes, re-index embeddings
```bash
cartog rag index .        # incremental — only new/changed symbols
cartog rag index . --force  # re-embed everything
```

After upgrading cartog, `rag index` auto-detects embedding format changes and re-embeds automatically.

### RAG query quality tips

| Query | Quality | Why |
|---|---|---|
| `"authentication token validation"` | Best | Describes behavior, multiple relevant terms |
| `"handle HTTP request errors"` | Best | Natural language, matches content in function bodies |
| `"config"` | Good | Single keyword works — FTS5 matches token in names and content |
| `"validate_token"` | Good | FTS5 matches the full token; use `cartog search` only if you need substring matching |
| `"parse"` | OK | Short queries return broad results; add context if too many hits |
| `"auth*"` | Bad | FTS5 wraps queries in quotes, disabling wildcards |

### Interpreting results

```
1. Function validate_token  auth/tokens.py:10-20  [fts5+vector] score=0.0328 rerank=8.61
2. Class AuthService  auth/service.py:1-11  [fts5] score=0.0164 rerank=-4.32
```

- `[fts5+vector]` — found by both keyword and semantic search (most confident)
- `[fts5]` — found by keyword search only
- `[vector]` — found by semantic similarity only (model must be set up)
- `score` — RRF rank score; only meaningful for ordering within one query
- `rerank` — cross-encoder relevance score (higher = more relevant). Shown when re-ranker model is available. Results are re-sorted by this score.
- If all results show `[fts5]` only, run `cartog rag setup && cartog rag index .` to enable vector search

### When RAG search returns nothing useful

1. Try simpler terms: `"token validation"` instead of `"JWT token validation with RSA signatures"`
2. Check that the index is built: `cartog rag index .`
3. Fall back to `cartog search` for name-based lookup
4. Fall back to `grep` for string literals or config values

## Token Budget

Use `--tokens N` to limit output when working with constrained context windows:
```bash
cartog --tokens 500 search validate
cartog --tokens 200 outline src/db.rs
cartog --tokens 1000 changes
```
Ignored with `--json`. Uses len/4 byte-to-token approximation.

## Tips

- Use `--json` when you need to parse output programmatically
- After making changes, run `cartog index .` to update (uses git to detect changes)
- Use `cartog index . --force` to rebuild the entire index from scratch
- `cartog search` matches symbol names (prefix + substring, case-insensitive)
- `cartog rag search` matches symbol names AND content (FTS5 tokens + vector similarity)
- For method queries, use the method name (e.g., `authenticate`), not dotted names
- RAG search does NOT do substring matching: `"valid"` won't match `validate_token` — use `cartog search valid` for that
