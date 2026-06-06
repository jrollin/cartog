# cartog-db

SQLite persistence layer for the cartog code graph.

## Overview

Stores symbols, edges, files, and embeddings in a single SQLite database. Provides query methods for graph traversal (callees, refs, impact, hierarchy), full-text search via FTS5, and vector similarity search via sqlite-vec.

## How it works

### Schema

Four core tables plus four RAG-specific tables:

- **`symbols`** — primary key on stable ID, indexed by `name` and `file_path`
- **`edges`** — `source_id` → `target_name`, with `target_id` resolved later. A `resolution_state` column tracks lifecycle: `0=unresolved`, `1=resolved`, `2=unresolvable` (LSP definitively gave up: typo, dyn dispatch, macro), `3=external` (LSP located the target outside the indexed root: stdlib, deps, node_modules). State 2 and 3 are sticky — skipped on future LSP passes until [`Database::reset_unresolvable_for_names`] reopens them on a name match, or [`Database::reset_all_unresolvable`] resets all of them on `--force`
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
4. **Parent scope** — first symbol sharing the source's parent scope (sibling, `LIMIT 1`)
5. **Project-wide unique** — exactly one match globally
6. **Kind disambiguation** — when exactly 2 matches remain, pick the higher-priority kind (type-like `class`/`interface`/`enum`/`type_alias`/`trait` > `function` > `method`); equal priorities stay unresolved

Tiers 5 and 6 are evaluated together in a single project-wide query (capped at 3 candidates): a lone match resolves via tier 5, exactly two resolve via the tier-6 kind disambiguation, and 3+ stay unresolved.

### Search ranking

Symbol search uses a composite score:

```text
rank = match_tier + kind_penalty
```

- **match_tier**: exact match (0), prefix (1), substring (2)
- **kind_penalty**: definitions `function`/`method`/`class` (0), `variable` and all other kinds (3), `import` (6)
- **tiebreaker**: `in_degree DESC` (most-referenced symbols first)

## Public API (key exports)

| Export | Description |
|--------|-------------|
| `Database` | Main handle — open, query, insert, resolve |
| `DB_DIR` / `DB_FILENAME` | Default DB location: `.cartog/db.sqlite` at the project root |
| `LEGACY_DB_FILE` | Legacy DB filename (`.cartog.db`), kept for backwards-compat lookups |
| `MAX_SEARCH_LIMIT` | Maximum results for search queries (100) |
| `UnresolvedEdge` | Edge pending LSP resolution |
| `IndexStats` | Aggregate statistics (files, symbols, edges, languages) |
| `DbError` / `DbResult` | Crate error type and result alias |
| `EmbeddingFingerprint` | Records the embedding strategy so a format change triggers re-embed |
| `checkpoint_wal()` | Force a WAL checkpoint (used before backups / handoff) |
| `CURRENT_SCHEMA_VERSION` / `DEFAULT_EMBEDDING_DIM` | Schema migration target and default vector dimension |

## Crate dependencies

`cartog-core`
