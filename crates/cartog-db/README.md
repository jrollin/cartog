# cartog-db

SQLite persistence layer for the cartog code graph.

## Overview

Stores symbols, edges, files, and embeddings in a single SQLite database. Provides query methods for graph traversal (callees, refs, impact, hierarchy), full-text search via FTS5, and vector similarity search via sqlite-vec.

## How it works

### Schema

Four core tables plus four RAG-specific tables:

- **`symbols`** — primary key on stable ID, indexed by `name` and `file_path`
- **`edges`** — stores `source_id` → `target_name`, with `target_id` resolved later
- **`files`** — tracks file hash, language, symbol count, last modified timestamp
- **`metadata`** — key-value store (e.g., `last_commit` for git-based change detection)
- **`symbol_content`** — raw source code per symbol (for FTS and embedding)
- **`symbol_fts`** — FTS5 virtual table over symbol names and content (BM25 ranking)
- **`symbol_embedding_map`** — maps integer rowids (for sqlite-vec) to symbol IDs
- **`symbol_vec`** — sqlite-vec virtual table with float32 vectors for KNN search. The vector dimension is configurable via `.cartog.toml` (default: 384). When the configured dimension changes, the vector table is automatically recreated.

### Edge resolution (6-tier heuristic)

When edges are first inserted, `target_id` is `NULL`. The resolution algorithm runs in **2 passes**, attempting to match `target_name` to a known symbol:

1. **Same file** — exact name match in the same source file
2. **Import path** — follow already-resolved import edges
3. **Same directory** — match symbols in sibling files
4. **Parent scope** — prefer symbols sharing the same parent
5. **Project-wide unique** — exactly one match globally
6. **Class over constructor** — when 2 matches remain, prefer `Class` kind

### Search ranking

Symbol search uses a composite score:

```
rank = match_tier + kind_penalty
```

- **match_tier**: exact match (0), prefix (1), substring (2)
- **kind_penalty**: definitions like function/class (0), variable (3), import (6)
- **tiebreaker**: `in_degree DESC` (most-referenced symbols first)

## Public API (key exports)

| Export | Description |
|--------|-------------|
| `Database` | Main handle — open, query, insert, resolve |
| `DB_FILE` | Default database filename (`.cartog.db`) |
| `MAX_SEARCH_LIMIT` | Maximum results for search queries (100) |
| `UnresolvedEdge` | Edge pending LSP resolution |
| `IndexStats` | Aggregate statistics (files, symbols, edges, languages) |

## Crate dependencies

`cartog-core`
