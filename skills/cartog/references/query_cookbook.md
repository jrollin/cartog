# cartog Query Cookbook

## Common Patterns

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

## Combining Commands

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

### Assess refactoring scope
```bash
cartog search OldClassName             # Confirm exact name and file first
cartog refs OldClassName               # All references
cartog hierarchy OldClassName          # Subclasses to update
cartog impact OldClassName --depth 5   # Full blast radius
```

## Semantic Search (RAG)

### Setup (one-time)
```bash
cartog rag setup          # download embedding + re-ranker models
cartog rag index .        # embed all symbols
```

### "Find code related to a concept"
```bash
cartog rag search "parse abstract syntax tree"
cartog rag search "handle HTTP authentication" --kind function
cartog rag search "database migration" --limit 5
```

### After code changes, re-index embeddings
```bash
cartog rag index .        # incremental — only new/changed symbols
cartog rag index . --force  # re-embed everything
```

### Good vs bad RAG queries

| Query | Quality | Why |
|---|---|---|
| `"authentication token validation"` | Good | Describes behavior, multiple relevant terms |
| `"handle HTTP request errors"` | Good | Natural language, matches content in function bodies |
| `"parse"` | Bad | Too short — use `cartog search parse` instead |
| `"validate_token"` | Bad | Looks like a symbol name — use `cartog search validate_token` |
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

## Tips

- Use `--json` when you need to parse output programmatically
- After making changes, run `cartog index .` to update (uses git to detect changes)
- Use `cartog index . --force` to rebuild the entire index from scratch
- `cartog search` matches symbol names (prefix + substring, case-insensitive)
- `cartog rag search` matches symbol names AND content (FTS5 tokens + vector similarity)
- For method queries, use the method name (e.g., `authenticate`), not dotted names
- RAG search does NOT do substring matching: `"valid"` won't match `validate_token` — use `cartog search valid` for that
