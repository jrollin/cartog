# Incremental Indexing

## 1. Why

Re-index latency dominates the editor save loop. Repos in the 10k–100k file range are common, and the goal is **a no-op re-index in well under a second** so that `cartog watch`, the MCP server, and on-demand `cartog index` runs feel instant.

Cartog reaches that goal with a layered change-detection pipeline: each layer prunes more aggressively than the next, so the expensive work only runs for the files (and ultimately the symbols) that actually changed.

## 2. The three layers

```
  ┌──────────────────────────────────────────────────────────┐
  │  cartog index <root>                                     │
  └────────────────────────┬─────────────────────────────────┘
                           │
              ┌────────────▼────────────┐
              │ Layer 1: git diff       │  skips: unchanged FILES
              │ (when .git available)   │  cost:  one git command
              └────────────┬────────────┘
                           │ no git? ↓
              ┌────────────▼────────────┐
              │ Layer 2: file SHA-256   │  skips: unchanged FILES
              │ (always runs as fallback)│ cost:  one read + hash
              └────────────┬────────────┘
                           │ file changed? ↓
              ┌────────────▼────────────┐
              │ Layer 3: symbol Merkle  │  skips: unchanged SYMBOLS
              │                         │  cost:  parse + hash
              └─────────────────────────┘
```

Each layer is a strict superset filter: **nothing reaches Layer N unless every layer above it said "changed"**. A file that git did not flag never gets re-hashed; a file whose SHA-256 matched its stored hash never gets re-parsed; a symbol whose `subtree_hash` matched never gets re-written to the DB.

## 3. Layer 1 — Git diff

When `.git` is available and a `last_commit` is stored in `metadata`, the indexer asks git which files changed since `last_commit`:

```text
git diff --name-only <last_commit> HEAD
git ls-files --others --exclude-standard
git diff --name-only            (working tree)
git diff --name-only --cached   (staged)
```

The union covers committed, staged, unstaged, and untracked changes. Files outside this set are skipped without being read from disk — the highest-leverage layer by far. Implementation: `crates/cartog-indexer/src/lib.rs` (`git_changed_files`).

Layer 1 returns `None` (forcing fallback to Layer 2) when:
- there is no `last_commit` row yet (first index),
- the repo has no `.git` directory or git is unavailable,
- the stored `last_commit` no longer exists (after a rebase or a forced reset).

## 4. Layer 2 — File SHA-256

For every file Layer 1 considered changed (or every file, when Layer 1 doesn't fire), the indexer reads the file and computes a SHA-256 of its contents. If the hash matches the value already stored in the `files` table, the file is skipped before parsing. This catches the common case of an editor saving a file with no actual content change (mtime updated, content identical).

Layer 2 also acts as the cold-start path: on first index, every file flows through it.

## 5. Layer 3 — Symbol Merkle

Once a file is parsed, every extracted symbol carries two hashes:

```
content_hash = SHA-256(kind || name || signature || body_source)
subtree_hash = SHA-256(content_hash || sorted child subtree_hashes)
```

`content_hash` covers what the symbol *is*. `subtree_hash` covers what the symbol *contains*. The two-hash split is what makes the diff surgical:

- `content_hash` matches, `subtree_hash` matches → symbol unchanged, no DB write.
- `content_hash` differs → symbol body or signature changed → `modified`.
- `content_hash` matches but `subtree_hash` differs → only a descendant changed → `children_changed`. The parent's row, edges, and `in_degree` counter are preserved untouched.

This matters for edge resolution and centrality: rewriting a parent every time a nested function changes would cascade into edge invalidation and unstable graph metrics.

Implementation: `crates/cartog-indexer/src/lib.rs` (`compute_merkle_hashes`, `merkle_diff`).

## 6. Why no directory layer?

Cursor's "Secure Codebase Indexing" article describes a Merkle tree at the file/directory level that prunes whole subtrees of the walk before reading any file. This was investigated for cartog and rejected. The reasoning is worth recording so the same idea doesn't get re-proposed.

A directory-level cache would only help if it could safely answer **"has anything under this dir changed?"** without reading every file. The candidate signal is the directory's own mtime — but on every filesystem cartog targets (APFS, HFS+, ext4, btrfs, NTFS), a directory's mtime updates only when its **immediate children's set of names** changes (add, remove, rename). Modifying a file's content **in place** does not bubble up to the parent's mtime.

So an mtime-keyed directory rollup would silently miss the most common edit a developer makes — saving a modified file. Walking into the directory and checking each file's content (or hash) is the only correctness-preserving option, which is exactly what Layer 2 already does. The "directory rollup" would add a cache that cannot be safely consulted.

Cursor's design works in their setting because they re-walk every file regardless; their Merkle tree is for *propagating* invalidation upward in a multi-tenant cloud index where access control is tied to per-block hashes. That model doesn't map to cartog's local-first, walk-everything-once design.

## 7. Schema

Tables involved in the incremental pipeline:

| Table | Layer | Key columns |
|---|---|---|
| `metadata` | 1 | `last_commit`, `schema_version` |
| `files` | 2 | `path PK`, `last_modified`, `hash`, `language` |
| `symbols` | 3 | `id PK`, `content_hash`, `subtree_hash` |
| `edges` | 3 | `source_id`, `target_id`, `kind`, `resolution_state` |

Column-level schema, the `resolution_state` lifecycle, and additional tables (RAG vectors, FTS5) live in [tech.md](../tech.md).

## 8. Failure modes & invariants

- **Permission errors.** A directory or file we cannot read is logged as a warning and skipped for this run; nothing about its prior cache state is invalidated.
- **Schema drift.** `CREATE TABLE IF NOT EXISTS` in the open path plus a versioned `migrate()` block guarantees old DBs upgrade transparently. Migration version bumps that wipe data trigger a backup first.
- **Force re-index.** `cartog index --force` bypasses Layers 1 and 2 entirely; Layer 3 still applies because every parsed symbol goes through Merkle diff regardless.
- **Phase 3 atomicity.** All DB writes during a single `index_directory` run participate in one outer transaction opened via `Database::begin_indexing_tx`. A panic, error, or hard process exit before commit rolls every write back — the index is never observed in a half-updated state where, for example, symbols have been re-inserted but the matching `files` row is stale. Helper methods come in two flavors: the public form (which opens its own transaction, used by callers who want a single-shot write) and an `_in_tx` variant (used by the indexer under `begin_indexing_tx`).

## 9. Not pursued

- Cross-user or cross-machine shared cache.
- Branch-aware caching (per-branch rollup snapshots).
- Multi-tenant access control over a shared index.
- Directory-level rollup — see §6 for why.

---

## See also

- [tech.md](../tech.md) — full architecture decisions and column-level schema
- [structure.md](../structure.md) — workspace layout and crate boundaries
- `crates/cartog-indexer/src/lib.rs` — pipeline implementation
- `crates/cartog-db/src/lib.rs` — schema, migrations, batch helpers
