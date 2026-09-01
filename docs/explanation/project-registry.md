# Project registry

**Status:** design proposal, not implemented.

A user-global registry of the cartog projects on a machine, so a single MCP
session can *discover* the other indexed projects — where their databases are,
what languages they hold, and what each project is for — without merging their
code graphs.

Motivating case: a microservice fleet, one repo (and one index) per service. An
agent working in `svc-billing` needs to answer "which service owns invoicing?"
before it can ask a question about that service's code.

## Scope

Three steps, each shippable alone:

| Step | Deliverable |
|------|-------------|
| 1 | `projects.sqlite` registry + `cartog projects list/forget/prune` |
| 2 | `cartog_list_projects` MCP tool, served from the existing per-project server |
| 3 | Self-populated `description`, so routing by intent works |

Explicitly **not** in scope (each has its own reasons, see
[Non-goals](#non-goals)): merging graphs into one database, federated search,
cross-service edges, a shared multi-project server, any mutation of another
project's index.

## Prerequisites

Two changes must land **before** any registry code is useful. Neither was
obvious until the crate boundaries were checked, and together they mean steps
1-3 are not the small additive change they first appear to be.

### P1 — a `cartog-registry` crate

`cartog::state::default_state_dir()` and `cartog::state::slot_for_db()` live in
the **binary** crate (`crates/cartog/src/state.rs`). `cartog-mcp` does not and
cannot depend on the binary — the binary depends on `cartog-mcp`, so it would
be a dependency cycle. The registry needs both helpers on both sides of that
boundary.

This also matches the codebase's stated architecture: lower crates never import
a config helper, which is why `allow_create` and `walk_filter` are threaded
into `run_server` as primitives from `main.rs`.

So: a new `cartog-registry` crate owning the registry schema, its read/write
API, and the slot hashing. Depended on by `cartog` and `cartog-mcp`.

Moving `slot_for_db`/`default_state_dir` out of the bin crate touches
`crates/cartog/src/{state.rs, main.rs}` and
`crates/cartog/src/commands/{index.rs, manage.rs, remote.rs}` (~15 call sites),
plus the doc comments in `cartog-watch` that name `cartog::state::slot_for_db`
by path. Per the refactoring rules this is a **move-only change that lands
separately** from the registry feature, preserving the public names and
signatures so callers do not change.

The alternative — threading a resolved project snapshot into `run_server` like
`allow_create` — avoids the new crate but forces `main.rs` to compute the list
at startup, so the MCP tool can never see a project registered later in the
session. Rejected for that reason.

### P2 — `cartog-db` must expose a drift-free version probe

The design needs to read a foreign project's `schema_version` **without** the
drift check, because `Database::open_readonly` returns `DbError::SchemaDrift`
on any mismatch in either direction — exactly the case worth reporting. But
`cartog-mcp` has no `rusqlite` dependency either; it reaches SQLite only
through `cartog-db`. So the "plain read-only connection" escape hatch is not
available to the consumers that need it.

Add to `cartog-db`:

```rust
/// Read `metadata.schema_version` from an existing DB without running
/// migrations or the drift check. For callers that must *report* on a
/// database this binary cannot open.
pub fn probe_schema_version(path: &Path) -> DbResult<u32>;
```

That is the right home regardless — it is a DB concern, and both `cartog` and
`cartog-mcp` need it.

## Design principles

1. **Metadata only.** The registry stores facts *about* indexes, never symbols,
   edges, or embeddings. Every project keeps its own `.cartog/db.sqlite` as the
   single source of truth for its graph.
2. **Derived, never authoritative.** The registry is a cache. Deleting it costs
   nothing but rediscovery; a stale row is a tolerated state, not corruption.
   Every consumer must work when the file is missing, unreadable, or stale.
3. **No new consent surface.** A project appears in the registry only as a side
   effect of a write it already consented to. Registration is never the reason a
   directory gets touched.
4. **Additive to the index.** No core schema change, no re-index, no change to
   the default `cartog serve` topology. Note this does **not** mean cheap: see
   [Prerequisites](#prerequisites) — the registry needs a new crate boundary and
   a `cartog-db` helper before any of it is usable from the MCP side.

## Step 1 — the registry

### Location

`<state_dir>/projects.sqlite`, where `state_dir` is
`cartog::state::default_state_dir()` — the same XDG-resolved directory that
already hosts `state.toml` and every PID lock file
(`crates/cartog/src/state.rs`). No new directory, no new platform logic.

`CARTOG_REGISTRY` overrides the path; setting it to an empty value disables the
registry entirely (both reads and writes).

### Schema

```sql
CREATE TABLE IF NOT EXISTS projects (
    id              TEXT PRIMARY KEY,  -- slot_for_db("serve", db_path)
    db_path         TEXT NOT NULL,     -- resolved, canonical where possible
    root            TEXT NOT NULL,     -- indexed project root
    name            TEXT,              -- defaults to root's basename
    description     TEXT,              -- step 3
    description_src TEXT,              -- 'config' | 'readme', step 3
    languages       TEXT,              -- JSON: [["rust",412],["markdown",30]]
    schema_version  INTEGER,
    symbol_count    INTEGER,
    edge_count      INTEGER,
    resolved_count  INTEGER,
    embedding_count INTEGER,
    embed_provider  TEXT,              -- from the DB's metadata; fingerprint
    embed_model     TEXT,              --   mismatch across projects blocks
    embed_dim       INTEGER,           --   federated semantic search
    source_fingerprint TEXT,           -- cheap identity of the source DB;
                                       --   equal => skip the write entirely
    last_indexed    INTEGER,           -- unix seconds
    last_seen       INTEGER            -- unix seconds, any registry write
);

CREATE TABLE IF NOT EXISTS metadata (key TEXT PRIMARY KEY, value TEXT);
```

`registry_version` in `metadata` gates future migrations. It is independent of
the core `SCHEMA_VERSION`: a registry bump must never force a re-index.

### Identity

`id` is `cartog::state::slot_for_db("serve", db_path)` — the existing
`sha256(canonical db path)[..8]` helper. Reusing it buys three things:

- Logically-equivalent paths (relative/absolute, symlinks, macOS `/tmp` →
  `/private/tmp`) collapse to one row, which is the correct outcome.
- The id **is** the serve PID-lock slot, so "does this project have a live
  server?" is a lookup in the state dir with no IPC.
- No second keying scheme to keep consistent.

Caveat, and it must be handled rather than assumed away: `slot_for_db` only
fully canonicalizes when the path exists, falling back to the nearest existing
ancestor and then to the raw path. A row written while the DB was absent can
carry an id that a later canonicalizable path would not reproduce. Therefore:

> **Consumers recompute the id from `db_path` at read time and treat a mismatch
> as "no live peer", never as a missing project.** The stored id is the primary
> key; it is not evidence about the filesystem.

### When rows are written

Registration piggybacks on writes that already happened, at the end of a
successful pass:

Each trigger writes **only the fields it already has in hand**. This is a hard
rule, not an optimization: `db.stats()` aggregates over `symbols` and `edges`,
so a trigger that calls it purely to register would add a real query to a hot
path.

| Trigger | Writes | Extra query? |
|---|---|---|
| `cartog index` (incl. `--force`) | full row: counts, languages, `last_indexed` | none — `cmd_index` already calls `db.stats()` (`crates/cartog/src/commands/index.rs`) |
| `cartog rag index` | `embedding_count`, `last_seen` | one cheap count; **must not** call `stats()` |
| `cartog serve` startup (primary or read-only attach) | `last_seen`; inserts a bare row if absent | none; **must not** call `stats()` |
| `cartog watch` re-index pass | counts, `last_indexed` (debounced; see below) | none — the pass already computed them |
| `cartog pull` (after the atomic swap) | full row: counts, languages, `last_indexed` | one `stats()` — unavoidable, and a pull already costs a network transfer |

Counts come from `IndexStats` (`crates/cartog-db/src/lib.rs`), which carries
`num_files`, `num_symbols`, `num_edges`, `num_resolved`, and `languages`. Note
this only makes registration free for `index` and the watcher, which have those
stats already; the other two triggers write a narrower row precisely so they
stay free.

The watcher writes on a re-index, which can be frequent. It updates the row at
most once per 60s, and never on a no-op pass (zero dirty files), so a busy
watcher does not turn the registry into a write-hot file.

**Skip an unchanged row by content hash.** Every trigger compares a cheap
identity of the source index — the main DB file's `(mtime_ns, size)`, or its
`data_version` pragma — against a `source_fingerprint` column, and writes
nothing when it matches. graphify does the equivalent with a SHA-256 of the
source `graph.json` and returns `skipped: true`; a hash comparison is a
stronger and cheaper guard than a time-based debounce, since it suppresses
redundant writes even when they arrive minutes apart. The 60s debounce stays as
a backstop for the case where the fingerprint *does* change on every pass.

Nothing else registers. Read commands (`search`, `refs`, `map`, …) do not, so
the registry never grows from a query.

### Consent

Registration requires no new gate because it cannot precede one. Every trigger
above is either a gated write command (`index`, `rag index`, `watch` — all
refused before dispatch without consent, per
[the consent gate](../reference/config.md#index-creation-consent-gate)) or a
`serve` that has opened a real DB. A degraded `serve` (no consent, in-memory DB)
**does not register**: there is no index to describe.

Consequence worth stating: a project in the registry is a project the user
already opted into indexing.

### Failure handling

The registry is best-effort on **both** paths:

- **Write failure** (missing state dir, read-only filesystem, lock contention,
  corrupt file) logs at `warn` via `tracing` and returns `Ok`. It must never
  fail an index, an embed, or a server startup. A code indexer that refuses to
  index because a metadata cache is unwritable would be strictly worse than one
  with no registry.
- **Read failure** distinguishes two cases, because they deserve different
  handling:
  - *Absent or unreadable* (no file, no permission) → an empty project list,
    reported as empty. Nothing is lost.
  - *Present but corrupt* (unparseable, or a failed integrity check) → **rename
    it to `projects.sqlite.corrupt.<unix-ts>`, report the path on stderr, and
    start fresh.** Never open-and-truncate, and never silently return empty: a
    corrupt file still holds every row the user accumulated, and discarding it
    without a trace destroys recoverable data and hides a real fault. The
    timestamped name also means a second corruption cannot clobber the first
    backup.
- **Concurrency**: N projects index in parallel, so writes use a short
  `busy_timeout` and a single-statement `INSERT ... ON CONFLICT(id) DO UPDATE`.
  No transaction spans more than one row. Contention bounds, the pragmas this
  file must *not* inherit from the project DB, and why it cannot deadlock are in
  [Process scenarios and the registry's own contention](#process-scenarios-and-the-registrys-own-contention).

### CLI

```
cartog projects list [--json]     # id, name, root, languages, counts, staleness, live peer
cartog projects forget <id|root>  # drop one row (does not touch the index)
cartog projects prune             # drop rows whose db_path no longer exists
```

`list` marks each row:

- `live` — a serve/watch PID lock matches the recomputed slot
- `stale-schema` — `schema_version` differs from `CURRENT_SCHEMA_VERSION`
- `missing` — `db_path` is gone (prune candidate)
- `embed-mismatch` — embedding fingerprint differs from the majority of
  projects that have embeddings (diagnostic; also the gate on federated
  semantic search)

`forget` never deletes an index. Removing a project's data means deleting its
`.cartog/`, which is the repo owner's call.

### Reading another project's database

Consumers must **not** reach for `Database::open_readonly` as the default.
It returns `DbError::SchemaDrift` when the stored `schema_version` differs from
the binary's, in *either* direction
(`crates/cartog-db/src/store/lifecycle.rs`). Since a stale-schema project is
exactly the case worth reporting, the primary read primitive cannot be the one
that refuses to open it.

Therefore:

1. Read `metadata.schema_version` with a plain read-only `rusqlite`
   connection.
2. If it matches, `open_readonly` is safe for richer queries.
3. If it does not, report the project from **registry-cached counts** plus the
   version, flagged `stale-schema`.

This makes the cached counts load-bearing rather than decorative: they are the
only data available for a project the current binary cannot open.

## Step 2 — `cartog_list_projects`

One read-only MCP tool, exposed from the **existing per-project server**. No
change to the serve topology — but note this is not free: it requires P1, since
`cartog-mcp` cannot reach the bin crate's `state` module where the state-dir
and slot helpers live today.

```jsonc
// input: { "include_stale": false }   // both optional
// output:
{
  "projects": [
    {
      "id": "serve-1a2b3c4d5e6f7a8b",
      "name": "svc-billing",
      "root": "/home/u/work/svc-billing",
      "db_path": "/home/u/work/svc-billing/.cartog/db.sqlite",
      "description": "Invoice generation and payment reconciliation.",
      "languages": [["rust", 412], ["markdown", 30]],
      "symbol_count": 8134,
      "resolution_rate": 0.71,
      "embedding_count": 8134,
      "last_indexed": "2026-08-28T09:14:22Z",
      "live": true,
      "stale_schema": false,
      "current": false          // true for the project this server serves
    }
  ],
  "registry_available": true
}
```

Design notes:

- **Read-only, no fan-out.** The tool reads the registry and the state dir (via
  `cartog-registry`, per P1). It does not open other projects' graphs, so it
  cannot be slow and cannot contend with another project's writer.
- **`current` is explicit**, so an agent knows which row it is already inside
  and does not re-route to itself.
- **`registry_available: false`** with an empty list when the registry is
  missing or unreadable — an honest empty, distinguishable from "no projects".
- **`db_path` is the payload that matters.** With it, an agent can run
  `cartog search --db <path>` or `cartog outline --db <path>` against another
  service. Cross-project capability arrives with **no new query engine** — the
  `--db` flag already does this.
- Compact by default, like the other MCP tools.

This raises the MCP tool count 16 → 17 and the CLI command count 27 → 28
(`projects`, with three subcommands).

### The intended two-step

```
cartog_list_projects            → route by description to svc-billing
cartog_search --db <that path>  → drill into that one graph
```

Route first, then drill. This is what makes step 3 the piece that gives the
registry its value, and it deliberately avoids merging result sets from
different graphs — see [Non-goals](#non-goals).

## Step 3 — self-populated description

A description is only useful if it stays true. So it is **authored in the repo
it describes** and refreshed whenever that repo is indexed. There is no global
file to hand-maintain and no separate staleness to track.

Two sources, in priority order:

1. **`[project] description` in `.cartog.toml`** — explicit, one line.

   ```toml
   [project]
   name = "svc-billing"
   description = "Invoice generation and payment reconciliation."
   ```

   See [The `[project]` config section](#the-project-config-section) for the
   full design — it touches more of the config machinery than a new struct
   field suggests.

2. **`README.md` read from disk** — fallback when no config description exists.
   The first non-empty prose paragraph under the top-level heading, truncated to
   280 characters at a word boundary.

   **Read the file directly; do not query the graph for this.** Markdown *is* an
   indexed language, but the markdown extractor stores no retrievable prose in
   the symbol: `crates/cartog-languages/src/markdown.rs` emits
   `SymbolKind::Document` whose `name` is the heading slug and whose `signature`
   is the heading text, with the body referenced only by byte offsets. The body
   text lands in `symbol_content`, which is (a) part of `RAG_SCHEMA` rather than
   the core schema, (b) written by the indexer's store phase through
   `extract_symbol_content_redacted`, so it is truncated and
   redaction-filtered rather than raw, and (c) reachable only on a
   `vec0`-capable connection in some paths.

   Reading `README.md` from disk is simpler, has no schema coupling, does not
   depend on RAG being enabled, and yields the actual prose. The graph stores
   *structure*, not retrievable prose — "it is already indexed, so reuse it" is
   the wrong instinct here.

`description_src` records which won, so `cartog projects list` can show that a
description was inferred rather than declared.

Rules:

- Extraction is **best-effort and never fatal.** No README, an empty one, or a
  badly-formed one leaves `description` NULL. A missing description is a normal
  state.
- A description is **plain text.** Markdown inline markup is stripped, not
  rendered. Newlines collapse to spaces.
- The description is **untrusted input.** It originates in repository content,
  so every consumer treats it as data: escaped on any HTML surface, never
  interpolated into SQL (parameterized like everything else), and never
  interpreted as instructions by an agent. A README is a plausible injection
  vector precisely because this field is designed to be read by a model. It is
  labeled as repository-authored content in the MCP output.
- Truncation is a hard cap enforced at write time, so the registry cannot grow
  unbounded from one pathological file.

## The `[project]` config section

A 9th top-level section in `.cartog.toml`. It is the only part of this design a
user writes by hand, so it is worth specifying against the real config
machinery rather than as one struct field.

### Shape

```toml
[project]
name        = "svc-billing"
description = "Invoice generation and payment reconciliation."
```

```rust
/// `[project]` — identity metadata for the project registry and
/// cross-project routing. Purely descriptive: nothing here changes how the
/// repo is indexed, walked, embedded, or queried.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfig {
    /// Display name in `cartog projects list` and `cartog_list_projects`.
    /// Defaults to the project root's directory basename.
    pub name: Option<String>,
    /// One-line summary of what this project is for. Read by agents to route
    /// between projects, so write it for that audience.
    pub description: Option<String>,
}
```

Both fields optional, so a bare `[project]` header is valid and inert. This
matches every sibling section (`SecurityConfig`, `IndexConfig`, … are all
all-`Option` with a `Default`).

### Why only these two fields

Deliberately minimal, and the omissions are the design:

| Rejected field | Why |
|---|---|
| `tags` / `keywords` | A description already carries this for a model reader, and a second free-text field invites drift between them. Add only if routing measurably fails without it. |
| `owner` / `team` | Not derivable from code, so it rots. Git history already answers "who works here". |
| `repo_url` | Derivable from `git remote`; a stored copy can contradict it. |
| `depends_on` (declared inter-service deps) | Tempting, and wrong at this layer: hand-declared edges rot silently, and step 5 exists to *derive* them from contracts. A declared dependency graph would compete with a derived one and be believed when stale. |
| `id` (a user-chosen stable id) | The registry id is `slot_for_db`, which is also the PID-lock slot. A second, user-settable identity would break that equality and invite collisions. |

The general rule: **`[project]` holds only facts a human must assert and a
machine cannot derive.** Anything derivable is derived.

### Integration points the config machinery demands

Adding a section is not just a struct field. Each of these is load-bearing, and
each has a test or a shipped bug behind it:

1. **`CartogConfig`** — add `pub project: Option<ProjectConfig>`
   (`crates/cartog/src/config/schema.rs`).

2. **`KNOWN_CONFIG_SECTIONS`** — add `"project"`
   (`crates/cartog/src/config/load.rs`). Miss this and every user with a
   `[project]` section gets `warning: unknown config key 'project'` on every
   interactive command, while the section still parses. A silent-nag bug.

3. **`deny_unknown_fields` on `ProjectConfig`** — so `descriptoin = "…"` is
   caught rather than silently ignored. This is what makes a typo actionable
   instead of a mystery empty description.

4. **The config-salvage path** — a typo inside `[project]` must degrade to
   *"this project has no description"*, never to a rejected config. The salvage
   machinery (`config/repair.rs`, `reparse_ignoring_unknown_keys`) already
   converges on any number of unknown fields by reading the offending key out
   of serde's error, so `[project]` inherits that behavior for free. Worth an
   explicit test anyway: **a broken `[project]` must not cost the user their
   index.** Note the consent interaction — `ConfigLoad::Rejected` still returns
   `IndexConsent::Granted`, so even a hard rejection does not un-consent a
   project; it only loses the description.

5. **`init.rs` `TOML_TEMPLATE` and the repo-root `.cartog.toml.example`** —
   **both**, and this is enforced: `every_template_key_parses_into_a_real_config_field`
   uncomments each template and parses it under `deny_unknown_fields`. That test
   exists because `[reranker] enabled` shipped in both templates for releases
   while no such field existed. A `[project]` block added to one template and
   not the other, or with a misspelled key, fails CI.

6. **`cartog config`** — display the resolved name and description. Note this
   command *bails* when the config was rejected rather than showing defaults, so
   it will not misreport a broken `[project]` as an absent one.

7. **`cartog doctor`** — a natural home for "this project has no description, so
   cross-project routing cannot see what it does". Advisory, never an error.

### Precedence and defaults

Name and description resolve independently, each highest-wins:

**Name:** `[project] name` → project root basename.

**Description:** `[project] description` → first prose paragraph of `README.md`
→ none. The registry records which source won in `description_src`
(`config` | `readme`), so `cartog projects list` can show that a description
was inferred rather than declared.

No env-var override for either. The sibling sections use env vars for
*operational* knobs (`CARTOG_JOBS`, `CARTOG_LSP_MAX_SERVERS`) where a CI run
legitimately differs from a laptop; a project's identity is not that kind of
setting, and an env-var name would just be another way to disagree with the
repo.

### Validation

- **Length caps at parse time**, not at write time: `name` ≤ 100 chars,
  `description` ≤ 280. A config that exceeds them is a rejected config with a
  message naming the field and the limit — better than silently truncating what
  the user wrote. (The README *fallback* still truncates, because there the
  user did not choose the length.)
- **Reject control characters and newlines** in both fields. A multi-line
  description breaks every single-line rendering surface, and the value flows
  into terminal output, JSON, and potentially HTML.
- **No credential-shaped keys check needed** here (unlike `RemoteConfig`, whose
  `validate_no_credentials` guards against secrets in TOML) — `[project]` has
  no field where a credential would plausibly be typed. Worth stating so the
  omission reads as considered rather than forgotten.

### Trust boundary

`description` is repository-authored text that exists to be read by an agent.
That makes it the most injection-prone field in the whole config, and the same
rules apply as to the README fallback: escaped on every rendering surface,
parameterized in SQL, labeled as repository-authored in MCP output, and never
treated as instructions. A `[project]` description is data.

### Tests

- a bare `[project]` header parses and yields no name/description
- `name`/`description` round-trip into the registry row
- an unknown key inside `[project]` is salvaged: the description is lost, the
  config still loads, and **the index is not refused**
- `"project"` is in `KNOWN_CONFIG_SECTIONS`, so a valid `[project]` emits no
  unknown-key warning (a regression test for integration point 2)
- over-length `name`/`description` are rejected with a message naming the field
- a description containing a newline or control character is rejected
- both templates (`init.rs` and `.cartog.toml.example`) carry the block and pass
  the existing parity test

## Session and agent launch model

How a row actually gets written, given how cartog is launched in practice.

### What launches the server

The Claude Code plugin (`.claude-plugin/plugin.json`) declares:

```json
"mcpServers": { "cartog": { "command": "cartog", "args": ["serve", "--watch"] } }
```

**No `--db`.** So each session's database is resolved from its **working
directory** — walk up to the first `.git`, use `<root>/.cartog/db.sqlite`. Two
consequences that define the whole model:

- **A session's project identity is its cwd**, not anything the agent chooses.
  There is no session id, no agent id, and nothing for a caller to get wrong.
- The plugin also wires a **SessionStart** hook (`ensure_indexed.sh`, which runs
  `cartog index .` in the background) and a SessionEnd hook.

### Many concurrent sessions

| Sessions | Same repo? | Result |
|---|---|---|
| 2 | yes | same DB, same `serve-<hash>` slot → first is primary, second attaches read-only |
| 2 | different repos | different paths → different slots → zero contention |
| 2 | one in a subdirectory | walk-up finds the same `.git` → **same row** (correct) |
| 2 | one in a git worktree | worktree's own `.git` file → **its own DB and row** (also correct) |

So a fleet of agent sessions needs no coordination logic from this design: the
per-DB election already handles same-repo overlap, and different repos never
interact.

### Why the shared registry stays safe under that fan-out

Three properties, all already specified above, restated here because this is
where they matter:

1. **Registration rides work that already happens.** The SessionStart hook
   indexes, which is the trigger that writes a full row. No new process, no new
   periodic task.
2. **Idle sessions write nothing.** Writes are edge-triggered (index, embed,
   pull, server start). Ten idle sessions produce zero registry traffic, so the
   steady state has no contention at any session count.
3. **No session owns the registry.** No PID lock, no slot, no promoter — see
   [the election note](#the-registry-does-not-participate-in-election). A
   read-only `serve` peer writes `last_seen` exactly like a primary does.

Worst realistic burst is an editor restart launching N servers at once: N
single-row upserts on distinct ids, bounded by the short `busy_timeout` and safe
to drop on contention.

### The one thing a `serve` start must do differently

`serve` startup inserting a *bare* row (id + path + `last_seen`) is not enough
when the DB already exists: the row would carry no counts, and
`cartog projects list` would show a registered project with nothing known about
it. So **when `serve` inserts a row for a DB that already exists, it reads the
counts once at startup.** One `stats()` against an existing index, at server
start, on the insert path only — not on the `last_seen` update path. This is
what makes the row useful immediately rather than after the next index, and it is
the mechanism that backfills most projects for free (next section).

## Backfilling existing projects

Registration is a side effect of a write, which leaves a real gap: **a project
indexed last month and untouched since never appears.** On the day this ships,
every existing user has databases and an empty registry, so
`cartog projects list` would read as broken.

### Rejected: scanning the filesystem

Walking the disk for `.cartog/` directories is the obvious fix and the wrong
one. It is slow, it is a surprising thing for a tool to do unasked, and it would
register repos the user never intended — a colleague's checkout, something under
`~/Downloads`. It also has no consent story, which matters for a tool whose
index-creation gate is deliberately explicit.

### Layer 1 — self-heal on touch (covers most projects, costs nothing)

Because the plugin launches `serve --watch` for every session, **opening a
project registers it**, with counts, per the previous section. No index needed —
the DB is already there. A project you actually work in appears the next time you
work in it.

This is the primary mechanism, and it matches prior art: Serena's global
`serena_config.yml` carries a `projects:` list, and its
`get_registered_project()` **auto-registers a project when its config already
exists** rather than requiring a separate step. Touch-to-register is the
established pattern, not an invention here.

### Layer 2 — explicit backfill for a whole fleet

For a user who wants everything visible now rather than repo-by-repo:

```
cartog projects add [PATH]                    # register one project, no indexing
cartog projects scan <DIR> [--depth N] [--dry-run]
```

- **`add`** resolves the DB exactly as every other command does and registers it
  if it exists. It **refuses rather than creating** when there is no index —
  registering a project cartog has never indexed would put a row in the registry
  describing nothing.
- **`scan`** walks a directory **the user names**. Never `$HOME` by default,
  bounded `--depth`, and `--dry-run` prints what it would add. The consent story
  is that the user typed the path.

### A backfilled row must be honest about what it does not know

A row created without indexing knows the DB's counts but not whether they
reflect current code:

- **`last_indexed`** comes from the DB if recoverable, else stays `NULL` and the
  listing renders `never (backfilled)` rather than implying freshness.
- **The `stale-schema` check still applies.** A month-old DB is the most likely
  to be on an older schema — so backfill is precisely the scenario where the P2
  version probe earns its keep, since `open_readonly` would refuse and the cached
  counts are the only data available. See
  [Reading another project's database](#reading-another-projects-database).

### What is not built

No background scan, no first-run migration that touches the filesystem, no
network discovery. Layer 1 plus an explicit Layer 2 is the whole story.

## The dashboard (`cartog dashboard`)

An **interactive, served** dashboard over the registry: filter, sort, search,
drill into a project, and see the view update as indexes change without
re-running a command.

This is the one part of the design that adds a runtime dependency and a network
listener, so its constraints are specified tightly.

### Shape

```
cartog dashboard                    # bind 127.0.0.1 on an ephemeral port, open a browser
cartog dashboard --port 7777        # fixed port (see single-instance below)
cartog dashboard --no-open          # print the URL, don't launch a browser
cartog dashboard --out report.html  # static snapshot; implies no server
```

`--out <PATH>` **implies the one-shot mode** — there is no separate `--once`
flag, because two flags for one mode is surface with no meaning. It writes a
self-contained file and **opens no socket at all**, which keeps a
zero-dependency escape hatch for CI and for attaching a report to a ticket. It
is also the only mode that cannot expose a port.

### Stack decision

**Server:** `axum`, on the `hyper` already vendored — with one qualification
that matters.

The **crate-level** delta is genuinely small. Against the current `Cargo.lock`:

| Already present | Newly added |
|---|---|
| `http`, `http-body`, `http-body-util`, `hyper`, `hyper-util`, `tower`, `tower-layer`, `tower-service`, `tower-http`, `bytes`, `futures-util`, `sync_wrapper`, `tokio-util`, **`subtle`** | `axum`, `axum-core`, `mime`, `matchit`, `serde_path_to_error` |

`subtle` being already vendored is convenient: the constant-time token compare
below needs no new dependency.

**But `hyper` is present client-only.** Today's enabled features are
`client`, `default`, `http1`, `http2` — **no `server`** — and it arrives
transitively via `hyper-rustls` → `reqwest`, not as a direct dependency
(`cargo tree -e features -i hyper`). So "already in the tree" is true at the
crate level and **not** at the compiled-code level: this change newly enables
`hyper/server`, compiling a body of code that ships in no current build. Budget
for that in binary size and compile time rather than assuming it is free.

All new crates must clear `cargo deny check` and `cargo audit` before landing,
per the dependency policy.

**Client: hand-written vanilla JS in one served HTML file. No JS framework, no
Rust UI framework, no build step.**

Considered and rejected:

| Option | Why not |
|---|---|
| **Dioxus desktop** | Needs system webview dev packages (Linux: `libwebkit2gtk-4.1-dev`, `libxdo-dev`, `libayatana-appindicator3-dev`, `librsvg2-dev`, `lld`). `cargo install cartog` cannot require apt packages, and cartog ships 4 prebuilt targets. A desktop app would have to be a **separate optional artifact** with its own release pipeline — a different product, not this feature. |
| **Dioxus/Leptos web (WASM)** | Interactivity requires `wasm32-unknown-unknown` **plus the `dx` CLI** in the release workflow (`dx build --web`), a second artifact type to ship or embed, and a WASM blob the browser must load. A second build toolchain across 4 targets for a page of cards, filters, and bars. |
| **`dioxus-ssr` only** | The one dependency-light Dioxus path (plain crate, `render_to_string`, no WASM/CLI) — but it renders a *static* string, which is exactly the thing interactivity rules out. |
| **npm + a bundler** | Cartog has no Node in its pipeline and should not gain one. graphify's `graph.html` does strictly more than this dashboard (force-directed layout, community filtering, click-to-inspect) with vanilla JS, one CDN library, and **no `package.json` at all**. |

The durable reason is not toolchain cost — that argument is weaker than it
looks, since the crate delta above is small. It is that **this page is read-only
over a single data source**, so there is no client-side state to manage beyond
view preferences (which filter is active, which cards are open). A framework's
value is coordinating derived state across many mutations; there are no
mutations here. The interactions wanted are DOM work over a JSON payload: filter
cards, sort by staleness or size, toggle a project open, render bars in CSS and
breakdowns in inline SVG. A few hundred lines of JS.

**Revisit the framework question if** the UI gains client-side state worth
coordinating — multiple views with routing, or any write path (triggering a
re-index from the page, editing config). Serving a read-only page is not that
threshold; the first mutation is.

### Live refresh without polling the DBs

The naive implementation re-reads every project DB on a timer. That would make
the dashboard the noisiest reader on the machine.

Instead: **watch the registry file, not the projects.** Every meaningful change
(an index finished, a pull landed, a server started) writes a registry row, so
the registry's own mtime is a change signal for the *listing*.

- **Transport: SSE** (`text/event-stream`), not WebSockets. One-directional
  server→client is all this needs, it is plain HTTP over the existing stack, and
  it reconnects on its own.
- **Debounce** registry-change events (≥500 ms) so a bulk re-index of 20 repos
  pushes one update, not 20.

#### The listing reads only the registry

**A refresh must not touch project databases.** The listing is served from the
registry alone — one small file, one read, no per-project connections. Only an
**expanded card** opens the project's DB, for the detail that is not cached
(top symbols by in-degree, provenance breakdown, live staleness).

This is what makes the rest of the design work:

- **Refresh cost is independent of project count.** Otherwise every event would
  re-read N databases, and with the LRU cap below N that means evict-and-reopen
  churn proportional to `projects × refresh_rate`.
- **Several browser tabs cost one read per event, not one per tab.** Each tab
  holds its own SSE stream, but they share the single registry read that
  produced the event.
- **The LRU residency cap becomes meaningful** — it bounds *expanded* cards,
  which a user opens a few of, rather than the whole fleet on a timer.

#### What registry-mtime does and does not cover

Stated plainly, because the signal is narrower than "live":

| Change | Refresh event? |
|---|---|
| An index/pull/embed finished (row written) | **yes** |
| A `serve` started or attached (`last_seen`) | **yes** |
| A re-index that changed nothing (`source_fingerprint` match → no write) | no — and correctly so, nothing to show |
| A repo being edited under `serve --watch`, between re-indexes | **no** |

That last row is the real limitation. A project's *staleness* changes
continuously as files are edited, but its registry row updates only on a
re-index, and the watcher debounces those to at most once per 60 s. So the
listing can lag true staleness by up to a minute.

This is acceptable and should not be papered over with polling: an expanded card
reads live staleness from that project's own DB on demand, and a dashboard that
updates on real events rather than a timer is the correct behavior. Polling every
project on an interval to shave that minute would make the dashboard the noisiest
reader on the machine — the exact thing this design avoids.

### It must not disturb the concurrency model

This is the part that matters most, and the rule is the same one that governs a
multi-project server:

> **The dashboard is a reader. It takes no PID lock, joins no election, spawns
> no watcher, runs no LSP, and never writes to a project DB.**

Concretely:

- Opens project DBs read-only, via P2's version probe first and
  `Database::open_readonly` only on a version match — a stale-schema project is
  rendered from cached registry counts, never a failed page. See
  [Reading another project's database](#reading-another-projects-database).
- **Does not write the registry either.** A dashboard that registered projects
  would turn a read surface into a write surface; it displays what other
  commands recorded.
- **Bounded residency.** If it holds project connections open at all, it uses
  the same pinned-plus-LRU discipline specified for the shared server
  ([residency](cross-project-queries.md#residency-must-be-bounded-pinned-default--lru)),
  keyed on `PRAGMA data_version` rather than file mtime — a WAL commit can leave
  the main file's mtime and size unchanged.
- Because it takes no lock, it cannot deadlock, cannot block a writer, and
  cannot be blocked by one beyond the `busy_timeout`.

### Security: a local port is an exposure

A listener is the first network surface cartog has ever opened. Requirements,
following the pattern graphify's `serve_http` arrived at:

- **Bind `127.0.0.1` by default.** Never `0.0.0.0` implicitly.
- **`--host` is opt-in and loud.** Binding a non-loopback address without a
  token is refused, not warned about. graphify only warns here; for a tool that
  serves source code, refusing is the right default.
- **Token by default, not optional.** Generate a random token per invocation,
  put it in the URL that is opened/printed, and require it on every request.
  Compare it in **constant time** (`subtle::ConstantTimeEq` or equivalent — note
  graphify uses `hmac.compare_digest` for exactly this) and treat a blank token
  as absent, never as "no auth required".
- **DNS-rebinding protection.** Validate the `Host` header against the bound
  address plus `localhost`/`127.0.0.1` (with and without the port). graphify
  does this via `allowed_hosts`; without it, a hostile page a user visits can
  reach a loopback listener.
- **No CORS.** Same-origin only; the page is served by the same process.
- **Read-only by construction.** No endpoint mutates anything — not the index,
  not the registry, not config. There is no write surface to authorize.
- **Bounded lifetime.** The server exits when the process is interrupted; it is
  a foreground command, never a daemon, and never auto-starts from a session
  hook.
- **It takes no PID lock — and this is a hard requirement, not a simplification.**
  `find_active_locks` scans *every* `*.pid` in the state dir (it filters on the
  extension alone), and `cartog self update` refuses on `active.first()` — **any**
  lock, not just serve/watch slots
  (`crates/cartog/src/commands/self_cmd/update.rs`). A `dashboard-<hash>.pid`
  would therefore make every self-update fail with `peer-running` for as long as
  a dashboard is open, breaking an unrelated and already-delicate subsystem. Do
  not add one.

#### Single-instance without a lock

Given the above, use **port binding itself as the mutex**:

- With `--port <N>`: if the bind fails because the port is in use, probe it and,
  if it is a cartog dashboard, print the existing URL and exit **0**. A user who
  runs the command twice gets their dashboard, not an error.
- Without `--port`: an ephemeral port is chosen, so two invocations both succeed
  on different ports. Harmless — each is an independent read-only view — and
  cheaper than coordinating.

This is lock-free, needs no state file, and gives the better UX than refusing.

### Rendering: the payload is untrusted

Symbol names, file paths, docstrings, and `[project] description` all originate
in repository content. graphify shipped a **stored XSS** through exactly this
path (its `#1838`: a node label containing a double quote, dropped into a quoted
`onclick`). Three defenses, all required:

1. **Escape `</` to `<\/` in every JSON blob embedded in a `<script>`.**
   `serde_json` alone does not prevent a `</script>` breakout from inside a
   string.
2. **Escape on the server at every HTML interpolation** — text and attributes
   both.
3. **Escape client-side before any `innerHTML`**, and **never emit inline
   `onclick`**. Use `data-*` attributes with one delegated listener on
   `document`, which is also what survives an `innerHTML` rebuild.

Prefer serving data as JSON over a fetch endpoint rather than embedding it in
the page: it removes defense 1's failure mode entirely, and it is what the live
refresh needs anyway.

### Degrade at scale rather than failing

graphify caps its visualization at 5,000 nodes
(`GRAPHIFY_VIZ_NODE_LIMIT`, `0` disables it for CI) and above the cap **builds
an aggregated community view instead of erroring** — returning `False` rather
than emitting a useless file when even that collapses.

The dashboard analogue: cap rendered rows per view, paginate or roll up above
it, and never attempt to render every symbol of a 50k-symbol project in a card.
A project that cannot be read at all renders as a card with an error badge; the
rest of the page still renders.

### External assets

**Fully inline: CSS, JS, and any icon.** Zero external requests, so the
dashboard works offline and on an air-gapped machine — which matches cartog's
local-first promise, and is the stronger form of what graphify does. If an
external asset ever becomes unavoidable, follow the `graph.html` pattern (exact
pinned version + SRI hash + `crossorigin`), not the `callflow_html.py` pattern
(floating major, no integrity).

### Tests

- a hostile symbol name (`"><script>alert(1)</script>`) round-trips through the
  page inert, asserted on the rendered bytes
- a request with no token, a wrong token, and a blank token are each 401
- a request with a foreign `Host` header is rejected (rebinding guard)
- `--host` with a non-loopback address and no token is refused before binding
- a stale-schema project renders from cached counts; the page still returns 200
- an unreadable project renders an error badge and does not fail the page
- the registry-change stream debounces N rapid writes into one event
- `--out <PATH>` writes a file and opens no socket
- `--port` on a port already held by a dashboard prints that URL and exits 0
- the dashboard writes nothing: no registry row, no project-DB write, and
  **no PID file** — asserted by scanning the state dir before and after, since a
  stray lock would break `cartog self update`
- a refresh event serves the listing without opening any project DB (assert on
  connection count, not just output)

## Interactions with existing cartog behavior

Six shipped behaviors move the DB file, wipe its contents, or duplicate it.
Each one has a consequence for a design keyed on the DB path and caching its
counts. None was accounted for in the first draft of this spec; all six are
requirements, not trivia.

### `cartog pull` replaces the DB file under the registry

`cartog pull` downloads to `<db>.partial` and then **atomically renames it over
`db_path`** (`crates/cartog/src/commands/remote.rs`), falling back to
copy+remove across device boundaries. Three consequences:

1. **Every cached count in the registry becomes wrong at once.** The row still
   points at a valid path, so nothing looks broken — the numbers just describe
   the pre-pull index. The `source_fingerprint` guard is what saves this: the
   replaced file has a different identity, so the next registry write refreshes
   rather than skipping. **Pull must therefore also register**, or a pulled
   index reports stale counts until its next local index.
2. **The slot hash survives**, because `slot_for_db` canonicalizes the *path*,
   which is unchanged. Good: the PID-lock slot and the registry id stay aligned
   across a pull.
3. **`pull` already takes both the `serve` and `watch` locks** for the duration
   (`acquire_pull_locks`), refusing unless `--force`. So a registry write from
   `pull` cannot race a `serve` startup write on the same row.

### The schema migration ladder can wipe the index

`migrate()` performs a destructive rebuild on some version transitions (the
2→3 stable-id wipe, the 6→7 symbol-id-escaping wipe), and
`backup_before_destructive_migration` copies the DB to a timestamped file
first. After a wipe the DB is **valid, current-schema, and nearly empty** until
the next index.

Consequence: a registry row can legitimately read `symbol_count: 0` for a
project that has plenty of code. `cartog projects list` must render that as
*"needs re-index"* rather than *"empty project"*, and must not treat a zero
count as a corruption signal. The timestamped backup file sitting next to the
DB is a useful hint that this is what happened.

### Git worktrees produce one registry row per checkout

DB-path resolution walks up to the first `.git` and uses `<root>/.cartog/`
(`crates/cartog/src/config/load.rs`). The check is `dir.join(".git").exists()`,
which matches a worktree's `.git` **file** as well as a real directory — so
each worktree resolves to its own root and its own DB, and therefore its own
registry row.

That is the correct default: two worktrees on different branches genuinely hold
different code and should not share an index. But it means:

- A developer with 3 worktrees of one repo sees **3 entries**, likely with the
  same `[project] name` from the shared `.cartog.toml`. `name` alone is not a
  unique display key; the listing must disambiguate by root path.
- This is the strongest argument for the deferred `git_id` column (open
  question 4): it would let a listing *group* worktrees of one repo while
  keeping their separate indexes. Grouping is a display concern, so it stays
  deferred — but the motivation is concrete, not hypothetical.

### A DB opened read-only cannot be registered from that process

`Database::open_readonly` exists for the single-writer secondary attach, and
write attempts surface as `SQLITE_READONLY`. A `serve` that lost the election
still wants to record `last_seen` — and it can, because **the registry is a
different file** from the project DB and is not covered by that connection's
read-only flag. Worth stating explicitly so an implementer does not
over-generalize "read-only attach" into "cannot touch the registry".

### `query_log` already records per-project read activity

The project DB logs one row per successful read tool call
(`query_log`, feeding `cartog savings`). This is why open question 3 resolves
to *"the registry must not write on reads"*: the recency signal already exists,
in the right place, at no extra cost. If per-project recency is ever wanted for
`prune`, read it from `query_log` rather than adding a registry write path.

### A rejected config still consents, but loses the description

`ConfigLoad::Rejected` returns `IndexConsent::Granted`
(`crates/cartog/src/config/load.rs`), so a broken `.cartog.toml` never
un-consents a project. Combined with the salvage path, the failure ladder for
`[project]` is:

| Config state | Index | Registry row | Description |
|---|---|---|---|
| Valid `[project]` | indexes | written | from config |
| Typo inside `[project]` (salvaged) | indexes | written | falls back to README |
| Config rejected outright | indexes | written | falls back to README |
| No config, but a DB exists | indexes | written | from README |
| No config, no DB, no `CARTOG_AUTO_INIT` | **refused** | **none** | — |

The invariant: **a description problem never costs a user their index.** Only
the consent gate does that, and it is unrelated to `[project]`.

## Process scenarios and the registry's own contention

The per-DB process/memory scenarios (mono-repo, monorepo, fleet, shared server)
are analysed in
[cross-project-queries.md](cross-project-queries.md#mcp-process-scenarios-mono-repo-multi-repo-fleet)
— including the measured idle footprints, the 60-process/5.5 GB fleet case, and
the rule that **any multi-project server is read-only**. That analysis is not
repeated here.

What *is* specific to this design: the registry is the **first file in cartog
that every project's processes write to in common**. Per-repo DBs never contend
with each other because they are separate files with separate locks. One shared
`projects.sqlite` inverts that, so its contention profile needs stating
explicitly.

### How many writers, how often

Worst realistic case, from the trigger table above: every project on the machine
indexing or starting a server at once — a `make`-driven bulk re-index, or an
editor restart that launches every configured `serve` simultaneously.

| Scenario | Concurrent registry writers | Write shape |
|---|---|---|
| One project indexing | 1 | one row upsert |
| 20 repos, bulk re-index | up to 20 | 20 row upserts, different `id`s |
| Editor restart, 20 configured servers | up to 20 | 20 `last_seen` touches |
| Steady state (servers idle) | **0** | nothing — no periodic writes |

Two properties keep this bounded, and both are deliberate:

- **No writer writes periodically.** Registration is edge-triggered by an index,
  an embed, a pull, or a server start. An idle fleet writes nothing at all, so
  the steady state has zero contention regardless of project count.
- **The `source_fingerprint` guard suppresses most writes before they happen.**
  A re-registration with an unchanged index is a read plus a comparison, not a
  write. Bulk re-indexing 20 repos where 18 are unchanged means 2 writes.

### Why this cannot deadlock

Same reasoning as the per-DB locks, and it must stay true:

- **One statement, one row, no transaction.** Every write is a single
  `INSERT ... ON CONFLICT(id) DO UPDATE`. There is no multi-statement
  transaction, so no writer holds the registry across another operation.
- **No process ever holds the registry and a project DB lock simultaneously in
  a way that could invert.** Registration happens *after* the indexing
  transaction commits, never inside it. This is a requirement, not an
  observation: writing the registry inside the index transaction would put a
  shared-file write inside a per-repo lock, creating exactly the ordering hazard
  that is currently absent.
- **Failure is not retried into a wait.** A `SQLITE_BUSY` past the timeout logs
  and returns `Ok` (the best-effort rule). A writer never blocks indefinitely,
  and never fails a user's index because a metadata cache was contended.

### Pragmas: do not copy the project-DB settings

The project DB opens with `journal_mode=WAL`, `busy_timeout=5000`,
`cache_size=-65536` (64 MB), and `mmap_size=268435456` (256 MB)
(`crates/cartog-db/src/store/lifecycle.rs`). Those are tuned for a
multi-hundred-megabyte code index.

Reusing them for the registry would be a bug at fleet scale: **20 processes each
mapping 256 MB for a file holding 20 rows.** Even unmapped-but-reserved address
space is the wrong signal, and the cache reservation is pure waste.

The registry should open with:

- `journal_mode=WAL` — attempt it, but **with a fallback** (see below);
  concurrent readers (`projects list`, an MCP tool) must not block a writer.
- **`journal_size_limit`** — cap it. SQLite's default is unlimited, which lets
  the `-wal` file keep the high-water mark of the largest-ever transaction
  forever. Hermes caps its session DB at 64 MiB for this reason; a registry
  holding tens of rows should cap far lower (1 MiB is generous). Without this, a
  one-off large write leaves a permanently inflated sidecar in the user's state
  dir.
- `busy_timeout` — **short**, on the order of 500-1000 ms rather than 5000. A
  contended metadata write should give up fast and log, not stall an index pass
  for five seconds. The best-effort contract makes giving up safe.
- `synchronous=NORMAL` — sufficient; a lost registry write is recoverable by
  re-indexing, unlike index data.
- **No `mmap_size` bump, no `cache_size` bump.** Defaults are correct for a
  file this small.

This is worth stating in the spec because the obvious implementation path is
"reuse `setup_opened_conn`", and that would silently import all four.

### WAL is not safe everywhere, and `$HOME` is the risky case

The project DB lives **inside the repo**, so it inherits whatever filesystem the
repo is on. The registry lives in the user's **state dir**, which in corporate
environments is routinely a network home directory. That is a materially
different risk profile, and the naive "just use WAL" answer is wrong there.

Hermes hit every variant of this in production with its own
`~/.hermes/state.db` and documents the failure modes
(`hermes_state.py`, the WAL-compatibility fallback block). They transfer
directly:

| Filesystem / condition | Symptom | Cause |
|---|---|---|
| NFS, SMB/CIFS, some FUSE, WSL1 | `SQLITE_PROTOCOL` — `"locking protocol"` | WAL needs shared-memory mmap + fcntl byte-range locks, which these do not reliably provide |
| Some FUSE mounts | `"not authorized"` | the WAL pragma is blocked outright |
| ZFS | `"disk i/o error"` | COW + mmap semantics can corrupt the `-shm` file under connection bursts |
| SQLite < 3.51.3 (without the 3.50.7 / 3.44.6 backports) | silent multi-process corruption | the documented WAL-reset bug |

Requirements for the registry:

- **Detect and fall back, warn once.** Match those error markers on the
  `journal_mode=WAL` pragma and fall back to `journal_mode=DELETE`. Concurrency
  drops (readers block during a write), but the feature works instead of every
  registry operation failing. Since the registry is best-effort by contract,
  degraded concurrency is a strictly better outcome than a hard failure — this is
  a cheaper trade for cartog than it was for Hermes, whose `/resume` and history
  depend on the same file.
- **Warn once per path, not per connection.** Hermes keeps a
  `_wal_fallback_warned_paths` set because a helper called from ~30 sites
  re-logged the same warning on every connection and filled the log. The
  registry is opened by every trigger, so it has the same shape.
- **Never live-downgrade an on-disk WAL database.** If the file is already in
  WAL, leave it: other processes (a `serve` peer, a concurrent index) may hold it
  open, and flipping `journal_mode` under them is unsafe. Only *fresh* or
  already-non-WAL databases get the fallback.
- **A registry that cannot be opened at all is not an error.** It degrades to
  "no registry" per the failure-handling rules — never a failed index.

Note the asymmetry worth stating: cartog's **project** DB already uses WAL
unconditionally (`crates/cartog-db/src/store/lifecycle.rs`), which is a
pre-existing exposure on network-mounted repos and out of scope here. This
section constrains only the new file.

### The registry does not participate in election

Stated explicitly to close the question: the registry has **no PID lock, no
slot, no primary, no promoter**. It is a plain SQLite file guarded only by
SQLite's own locking. Nothing about it can create a master/slave relationship,
and a read-only `serve` peer writes `last_seen` to it exactly like a primary
does — the read-only flag applies to that peer's *project DB* connection, not to
a different file.

## Limitations

Stated plainly, because each is a thing a user will hit and none is a bug:

- **The registry is machine-local and single-user.** It lives in the user's
  state dir, is not synced, and is not shared between accounts. Two users on one
  machine get separate registries; one user on two machines gets two. There is
  no design here for a team-wide project catalog, and adding one would need a
  different trust model than a local file.
- **A project is invisible until it is indexed once.** Registration is a side
  effect of a write, so a repo you have never indexed does not appear — even if
  it sits next to ten that do. There is no discovery-by-scanning, deliberately:
  walking the filesystem looking for `.cartog/` directories would be both slow
  and a surprising thing for a tool to do unasked.
- **Counts are as fresh as the last index, not as fresh as the code.** The
  registry caches; it does not poll. A repo edited but not re-indexed reports
  its previous numbers. `last_indexed` is the honest signal, and the listing
  should lead with it.
- **Descriptions are only as good as the repos' own README/config.** A fleet
  where every service README opens with a build badge and a license line yields
  a fleet of useless descriptions, and routing degrades to guessing from names.
  The `description_src` field at least makes "nobody wrote one" visible.
- **No cross-project graph queries.** Steps 1-3 deliver discovery only. `refs`,
  `impact`, and `trace` stop at the repo boundary and will keep doing so until
  cross-service edges exist — see
  [cross-project-queries.md](cross-project-queries.md).
- **Two projects can share a `[project] name`.** Nothing enforces uniqueness
  (worktrees make it likely), so `name` is a display label and never an
  identifier. The id stays `slot_for_db`.
- **The liveness flag is best-effort.** It reads PID files, which a crashed
  process leaves behind until the next reader reaps them, and the slot is
  recomputed from a path that may not canonicalize identically. Treat it as a
  hint, never as a lock.

## Why metadata-only rather than a merged graph

An earlier draft of this design asserted that merging per-repo graphs into one
user-global store was simply *wrong*, on the grounds of namespace bleed: within
a single monorepo, unrelated symbols already cross-resolve via the
`unique_global` tier, and a merged store would extend that failure across
repos.

That reasoning was too strong. Merging **is** viable, and there is a working
implementation to learn from: graphify keeps a merged
`~/.graphify/global-graph.json` alongside a `global-manifest.json` of tracked
repos. Two mechanisms make it safe there, and either would work here:

1. **Namespace every id on merge.** Its `prefix_graph_for_global` rewrites each
   node id to `<repo_tag>::<id>`, preserves the display label, and keeps a
   `local_id` so the original is recoverable. Collision between repos becomes
   impossible by construction rather than improbable.
2. **Forbid cross-repo similarity matching outright.** Its dedup path *raises*
   when nodes span more than one repo, on the reasoning that repos share label
   names by coincidence and must never be merged by string similarity. That is
   precisely the `unique_global` bleed, guarded by an assertion instead of an
   argument.

A third lesson transfers directly: on merge, **any per-graph integer identifier
must be re-based, not carried**. graphify shifts community ids by a per-repo
offset because every graph numbers its communities from 0, so unchanged ids
fuse unrelated communities into one meta-node. Cartog's analogue is `in_degree`
(and any future per-graph ranking integer) — the same hazard, described for
search ordering in
[cross-project-queries.md](cross-project-queries.md#the-caveat-indegree-is-per-graph-and-it-dominates).

So the choice here is a trade, honestly stated:

| | Merged store | Metadata-only (this design) |
|---|---|---|
| Freshness | a **snapshot**. Re-merge can ride the index itself (graphify's `--global` flag re-adds after a build) and a hash match skips it, so staying current is automatable — but it is still a copy that is only as fresh as its last merge | always reads the live per-repo DB; nothing to reconcile |
| Cross-project query | works offline against one artifact, no fan-out, no score merging | needs fan-out (step 4) or route-then-drill (step 2) |
| Per-repo index stays the only authority | no — the merged copy can diverge | yes |
| Stale entry after a repo is deleted or moved | prune the global store (drops its nodes) | prune the registry row (drops metadata only) |
| Schema drift across repos | silently merges mismatched versions | detected per project (P2), reported |
| Cost of being wrong | a query returns stale or mismatched graph data | a listing shows a stale count |

Cartog takes metadata-only for one decisive reason: **the blast radius of a
stale entry differs by an order of magnitude.** A stale row in a metadata
registry misreports a symbol count in a listing. A stale region of a merged
graph answers a code question with symbols that no longer exist — and an agent
cannot tell the difference. Given a code index whose whole value is being
current, and a per-repo `.cartog/` that is already the unit users delete,
gitignore, and sync to S3, the copy is not worth the risk.

That is a reasoned preference, not a prohibition. If a merged view is ever
wanted — and for genuinely offline cross-repo analysis it may be the right
tool — the two safeguards above (id namespacing, no cross-repo similarity
matching) plus per-graph integer re-basing are the prerequisites, and the schema
check from P2 should gate the merge so mismatched versions are refused rather
than silently combined.

## Non-goals

| Not doing | Why |
|---|---|
| Merging graphs into one global DB | A deliberate choice, **not a prohibition** — see [Why metadata-only rather than a merged graph](#why-metadata-only-rather-than-a-merged-graph). Metadata-only keeps every query reading live per-repo data and avoids a second staleness surface, and preserves per-repo `.cartog/` deletability, gitignore, and per-project S3 sync. |
| Federated `rag_search` | `hybrid_search` binds to one `Database`. RRF ranks and in-degree centrality are per-graph, and the embedding fingerprint (`embedding_provider`/`embedding_model`/`embedding_dimension` in `metadata`) is per-DB — two projects embedded differently hold vectors in different spaces, so merging their scores is meaningless. Needs a cross-DB normalization proven against the RAG relevancy benchmark. |
| Federated exact-symbol `search` | Tractable (names match or they don't, no score merging), but out of scope here. Step 2 already enables it via `--db`. |
| Cross-service edges (endpoint → handler) | The prize for microservices, and a separate feature: needs a contract layer (OpenAPI/protobuf) as the join key. A per-repo graph sees only a URL string on one side and a route handler on the other. |
| Shared multi-project MCP server | Its only real benefit is sharing loaded models across projects. Gated on measuring N × idle RSS, since it trades away crash isolation, per-DB election, and LSP isolation. Steps 1-3 do not need it. |
| Mutating another project's index | A registry entry grants discovery, not write access. |

## Testing

Per the repo's testing rules, each behavior ships with a test.

Unit (`cartog` crate):

- a row round-trips: insert, read back, field-for-field
- two logically-equivalent `db_path` forms (symlinked, relative) produce one row
- `ON CONFLICT` updates rather than duplicating
- an unwritable registry directory logs and returns `Ok` — the caller's index still succeeds
- a missing registry yields an empty list with `registry_available: false`
- `prune` drops exactly the rows whose `db_path` is gone
- `forget` leaves the index on disk untouched
- description precedence: config wins over README; neither yields NULL
- a project whose embedding fingerprint differs from the majority is flagged
  `embed-mismatch`; a project with no embeddings is not flagged
- re-registering an unchanged index writes nothing (`source_fingerprint` match)
- a corrupt registry file is renamed to `.corrupt.<ts>` and the original
  contents remain on disk; the caller's command still succeeds
- `serve` startup against an **existing, unregistered** DB writes a row *with
  counts*, not a bare row
- `serve` startup against an already-registered DB updates `last_seen` only and
  does **not** call `stats()`
- `projects add` on a path with no index refuses and writes no row
- a backfilled row with no recoverable index time renders as `never
  (backfilled)`, not as fresh
- a registry on a filesystem that rejects the WAL pragma falls back to
  `journal_mode=DELETE` and still round-trips a row
- an already-WAL registry file is never live-downgraded
- description truncation holds the cap and cuts at a word boundary
- a README containing HTML/Markdown markup yields stripped plain text

Integration:

- `cartog index` on a fresh project registers it; counts match `cartog stats`
- a **degraded** `serve` (no consent, no DB) registers nothing
- a project whose `schema_version` is bumped underneath reports `stale-schema`
  from cached counts rather than failing the whole listing
- a live `serve` makes its project report `live: true`; after it exits, `false`
- two concurrent `cartog index` runs on different projects both register

Seam test (the invariant a refactor could break): registration is a *side
effect* of a successful write. A failed or rolled-back index must leave no row,
and must not update an existing row's `last_indexed`.

## Documentation surface

Mandatory in the same change, per the docs + site-sync rule:

- `docs/reference/cli.md` — `cartog dashboard` (+ `--port`/`--host`/`--no-open`/
  `--once`/`--out`), and `cartog projects` with its **five** subcommands
  (`list`, `forget`, `prune`, `add`, `scan`), incl. the `live` / `stale-schema` /
  `missing` / `embed-mismatch` markers
- `docs/reference/config.md` — the `[project]` section (9th section)
- `crates/cartog/src/config/load.rs` — `KNOWN_CONFIG_SECTIONS` gains `"project"`
- `.cartog.toml.example` at the repo root **and** `init.rs` `TOML_TEMPLATE` —
  both, or the template-parity test fails
- `docs/reference/mcp-tools.md` — `cartog_list_projects` (16 → 17 tools)
- `docs/explanation/project-registry.md` — this document
- `docs/explanation/README.md` **and** `docs/README.md` — both indexes list every
  explanation doc; the convention requires keeping them in sync when adding one
- `docs/usage.md`, `docs/product.md`, `docs/structure.md`, README — command
  count 27 → 28, tool count 16 → 17
- `AGENTS.md` / `CLAUDE.md` — counts, and the registry in Current State
- `skills/cartog/SKILL.md` — the route-then-drill pattern
- `site/src/pages/index.astro` and `usage.astro` — `[project]` needs **both** a
  summary-table row and an explanation + example block; counts updated
- `crates/cartog/src/commands/init.rs` — `[project]` in the template comment
- `claude plugin validate .` after the plugin surface changes

The counts appear in several files; search for the previous value and bump every
occurrence rather than the first.

## Open questions

1. **Does anything query across projects in practice?** The registry is partly
   an instrument to find out: it reveals how many projects a real user has and
   whether routing gets used. Steps 4-7 (federated search, shared server) should
   wait on that evidence.
2. **Do a user's projects share an embedding fingerprint?** Promoted out of this
   list — it is cheap enough to just do. The three keys
   (`embedding_provider`, `embedding_model`, `embedding_dimension`) already live
   in each DB's `metadata`, so the registry caches them and
   `cartog projects list` flags any project whose fingerprint differs from the
   majority. This is a useful diagnostic on its own (a project silently embedded
   with the wrong provider gets no vector recall), and it is the gate on
   federated semantic search — see
   [cross-project-queries.md](cross-project-queries.md#step-6--federated-semantic-search).
   Add `embed_provider`, `embed_model`, `embed_dim` to the schema.
3. ~~Should `last_seen` be written by read commands?~~ **Resolved: no.** The
   recency signal is not worth a write on every query, and `prune` already has a
   better predicate (`db_path` no longer exists) that needs no recency at all.
   Note the DB *does* already log read-tool calls into `query_log` for
   `cartog savings`, so per-project recency is recoverable from the project's own
   DB if it is ever wanted — without the registry taking a write.
4. **Git identity as a stable id.** `slot_for_db` keys on the DB path, so moving
   a repo creates a second row and `prune` reaps the old one — the failure mode
   is a stale row until the next prune, not a wrong answer, which is why this
   stays deferred. Hashing the git common dir (grepai's `WorktreeID`) would
   survive a move and unify worktrees under one identity, but it adds a second
   keying scheme *and* breaks the id-equals-PID-lock-slot property that makes
   liveness free. If it is ever wanted, it belongs as a separate `git_id`
   column, not as a replacement for `id`.
