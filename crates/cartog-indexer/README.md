# cartog-indexer

Code indexing and change detection for cartog.

## Overview

Walks a directory tree, detects which files changed, extracts symbols and edges via `cartog-languages`, and writes the results to `cartog-db`. Indexes both code files and Markdown documents (`.md`). Uses a multi-tier change detection strategy and Merkle tree hashing for surgical symbol-level updates.

> Architecture overview: [docs/architecture/incremental-indexing.md](../../docs/architecture/incremental-indexing.md). This README focuses on the crate's public API and module layout.

## How it works

### Change detection (3 tiers)

1. **Git-based** — `git diff last_commit..HEAD` identifies changed files; unchanged files are skipped without reading them from disk
2. **SHA-256 fallback** — for non-git repos or when git detection fails, each file is hashed and compared to the stored hash
3. **Force mode** — `force=true` bypasses all checks and re-indexes everything

### Merkle tree hashing

Each symbol gets two hashes for fine-grained diff detection:

```
content_hash = SHA256(kind + ":" + name + ":" + signature + ":" + body_source)
subtree_hash = SHA256(content_hash + sorted(children_subtree_hashes))
```

Computed bottom-up (post-order traversal). When re-indexing a file, symbols are classified as:

- **added** — new symbol ID not in previous index
- **modified** — `content_hash` changed (body or signature edited)
- **children_changed** — `content_hash` same but `subtree_hash` differs (child added/removed/modified)
- **unchanged** — both hashes match, skip entirely
- **removed** — symbol ID in old index but not in new extraction

Edges are always fully re-inserted for dirty files (no edge-level diff).

### LSP resolution (optional)

When the `lsp` feature is enabled, a second pass resolves edges that the heuristic resolver in `cartog-db` left unresolved, using real language servers via `cartog-lsp`. Skipped on no-op reindexes (no file added, modified, or removed) — the unresolved set is identical to the previous run. Use `--force` to retry resolution after toggling `--no-lsp` off.

Edges that LSP classifies are persisted to `resolution_state` so future runs skip them: `2` (unresolvable — typo, dyn dispatch, macro expansion) and `3` (external — stdlib, deps, node_modules). When a new symbol is added whose name matches such an edge (e.g. the user vendors a dep in-tree), the indexer auto-resets the marker so the next pass retries — see `Database::reset_unresolvable_for_names`. `--force` resets all sticky markers (see `Database::reset_all_unresolvable`).

## Public API

| Export | Description |
|--------|-------------|
| `index_directory()` | Main entry point — index a directory into the database. Optional `progress: Option<ProgressCallback>` fires at coarse phase boundaries (`Walking`, `Parsing`, `Storing`); pass `None` for the no-op default. |
| `IndexResult` | Summary: files indexed/skipped/removed, symbols added/modified, edges resolved |
| `ProgressUpdate` / `ProgressCallback` | Plain-data callback types for in-flight phase reporting (transport-agnostic) |
| `is_ignored_dirname()` | Check if a directory name should be skipped (`.git`, `node_modules`, `target`, etc.) |
| `git_recently_changed_files()` | List files changed in the last N git commits |

## Crate dependencies

`cartog-core`, `cartog-db`, `cartog-languages`, optionally `cartog-lsp`
