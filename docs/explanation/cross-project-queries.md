# Cross-project queries

**Status:** design proposal, not implemented. Rougher than
[the project registry](project-registry.md) — steps 4-7 are sketched to fix
their boundaries and dependencies, and each needs its own review before
implementation. Marked `[NEEDS REVIEW]` where a claim rests on an unverified
assumption.

Steps 1-3 (registry, `cartog_list_projects`, descriptions) are specified
separately in [project-registry.md](project-registry.md). This document covers
what comes after, and deliberately gates most of it on measurement.

| Step | What | Cost | Gate |
|------|------|------|------|
| 4 | Federated exact-symbol `search` | medium | steps 1-2 |
| 5 | Cross-service contract edges | large | own design; the differentiating feature |
| 6 | Federated `rag_search` | medium-large | needs cross-DB score normalization + benchmark proof |
| 7 | Shared read-only multi-project server | medium | needs N × idle-footprint measurement |

The ordering is not a roadmap. Step 5 is the most valuable and the most
independent; steps 6 and 7 may never be built, and the doc says under what
evidence they should be dropped.

## Inherited prerequisites

Steps 4, 6, and 7 all read foreign project databases, so they inherit both
prerequisites from [project-registry.md](project-registry.md#prerequisites):

- **P1, a `cartog-registry` crate.** `default_state_dir`/`slot_for_db` live in
  the binary crate and `cartog-mcp` cannot depend on it (that would be a
  dependency cycle). Any MCP-side federation needs them factored out first.
- **P2, `cartog_db::probe_schema_version`.** Reading a foreign DB's schema
  version without the drift check. `Database::open_readonly` refuses on any
  mismatch in either direction, and `cartog-mcp` has no `rusqlite` dependency of
  its own — it reaches SQLite only through `cartog-db` — so the "plain
  read-only connection" workaround is unavailable to the crate that needs it.

P2 is what makes per-project degradation implementable at all: without it, a
federated query cannot distinguish "this project is on an old schema" from "this
project failed to open", and cannot report the former while still answering from
the rest.

Step 5 does not inherit these (it touches the indexer, not cross-DB reads).

## Step 4 — federated exact-symbol search

> **Status: implemented.** `cartog search --all` (CLI) and `cartog_search_all`
> (the 18th MCP tool). Resolved as designed below: fan out to each project's
> database read-only, group results per project, no merged ranking and no
> merged database. [Open question 1](#open-questions) was settled by taking the
> grouped presentation, which needs no ranking benchmark. Filters are
> `--under <path>` and `--lang <lang>`, both keyed on columns the registry
> already stores (so no schema change), with `--max-projects` (default 10)
> bounding the fan-out.
>
> The select-and-query logic is **duplicated** between
> `crates/cartog/src/commands/search_all.rs` and
> `crates/cartog-mcp/src/tools/search.rs`: no existing crate can host it, since
> `cartog-registry` deliberately carries no `cartog-db` dependency and
> `cartog-db` depends only on `cartog-core`. Keep the two in step.
>
> Measured on a real 53-project fleet: a full fan-out opens every database
> read-only in ~117 ms, so the cap and filters are about relevance and response
> size rather than latency.

### Why this one is tractable

Symbol search ranks by a **deterministic, absolute formula**, not a per-DB
relative score. `Database::search` (`crates/cartog-db/src/store/queries.rs`)
computes `rank = match_tier + kind_penalty`, where `match_tier` is 0/1/2 for
exact/prefix/substring and `kind_penalty` is 0/3/6 by symbol kind. Those values
mean the same thing in every database.

So merging N result sets is a genuine merge, not a guess: sort by the same
`rank`, and the combined ordering is exactly what a single database holding all
N would have produced — with one caveat below.

This is the property `rag_search` lacks (step 6), and it is why step 4 can ship
long before step 6 is even designed.

### The caveat: `in_degree` is per-graph, and it dominates

The `ORDER BY` is `rank, in_degree DESC, is_test, kind, file_path, start_line`
(`crates/cartog-db/src/store/queries.rs`). `rank` is absolute; **`in_degree` is
not** — it counts references within one graph, so a large service's symbols
carry systematically higher values than a small one's.

The severity here is easy to understate. `rank` is **coarse**: `match_tier` is
0/1/2 and `kind_penalty` is 0/3/6, so `rank` takes only a handful of distinct
values across all results. Ties on `rank` are therefore the **common case, not
the edge case**, which means the per-graph, incomparable `in_degree` decides
most of the ordering in practice — it is the dominant ordering factor, not a
tie-break.

That promotes option 1 below from "probably right" to **the only defensible
default without a ranking benchmark**. Options:

1. **Group by project in the output**, so no cross-project tie-break is needed.
   Results are presented per project, each internally correctly ordered. This
   sidesteps the problem instead of solving it, and is probably the right
   answer for an agent audience, which wants to know *which service* a symbol
   lives in.
2. Normalize `in_degree` to a per-project percentile before merging. More
   faithful to a single-DB ordering, but it is a real ranking change that needs
   a benchmark to justify. `[NEEDS REVIEW]`
3. Ignore `in_degree` across projects and tie-break on project name. Simple,
   deterministic, and slightly worse within each project.

Recommendation: **option 1**, and not merely on preference — a flat merge is
not defensible until a benchmark shows the normalization in option 2 preserves
relevance. Defer the flat variants until someone demonstrates a flat list is
what they want *and* a measurement backs the ordering.

### Surface

```
cartog search <name> --all-projects [--projects a,b,c]
```

and the MCP equivalent as an optional argument on `cartog_search`, absent =
current behavior.

Mechanics:

- Resolve target DBs from the registry (step 1).
- Open each read-only. **Per-project degradation is mandatory:** a project whose
  `schema_version` differs cannot be opened by `Database::open_readonly` (it
  returns `DbError::SchemaDrift` in either direction), so it is reported as
  skipped-with-reason and the other projects still answer. A federated query
  must never fail wholesale because one project is stale.
- Every result carries its project id and name, so an agent can drill in.
- Start **serial**: `search` is one indexed query per DB, and N is small (a
  fleet is tens of repos, not thousands). Add concurrency only if a measured
  fan-out exceeds a latency budget — and note the ordering is deterministic
  either way, since results sort by absolute `rank` (and by project under
  option 1), not by arrival. Per the measure-before-and-after rule, a
  concurrency change here needs a before/after number, not an intuition.

### Which tools generalize

| Tool | Federates? | Note |
|---|---|---|
| `search` | yes | absolute rank formula |
| `outline` | no | inherently one file |
| `refs`, `callees`, `impact`, `trace`, `hierarchy` | no | traverse edges, which stop at the repo boundary — cross-project traversal needs step 5 |
| `stats`, `map` | aggregate only | a rollup, not a merge |
| `rag_search`, `context` | no | step 6 |

Worth stating plainly: **graph traversal does not federate.** `impact` across
services is impossible until cross-service edges exist, which is step 5. Step 4
is search only, and the doc should not imply otherwise.

### Prior art: nobody federates live

Worth recording, because it is evidence rather than opinion. graphify supports
multiple projects two ways, and **neither is live federation**:

- Its multi-project MCP server routes **one call to one graph** via an optional
  `project_path` argument. No fan-out, no merged result set.
- Its cross-project *querying* is achieved by merging graphs **offline** into
  `~/.graphify/global-graph.json` — and that merged artifact is not wired into
  the MCP server at all; it is a CLI-only concern.

So the one shipped tool in this space reaches cross-project capability by
route-then-drill (step 2 here) or by offline merge, and avoids merged-ranking
fan-out entirely. That is consistent with the analysis in step 4: the merge is
the hard part, not the fan-out. It also means a federated ranked result set
would be novel rather than catch-up work — which raises the bar for proving it,
not the reward for shipping it.

## Step 5 — cross-service contract edges

The prize for a microservice fleet, and the one feature here that would
differentiate cartog rather than catch up to a competitor.

### The gap

Today an HTTP call from service A to service B is invisible. A's graph sees a
URL string; B's graph sees a route handler. No edge connects them, so `impact`
stops at the repo boundary. The question a fleet owner actually has — *if I
change this endpoint, which services break?* — is structurally unanswerable.

This cannot be recovered by merging per-repo graphs. The join key is not in
either graph.

### The join key: contracts

A shared contract artifact is what both sides reference:

- **OpenAPI / Swagger** — HTTP. Operation id or method+path is the key.
- **Protobuf / gRPC** — service + method. Strongest case: both sides generate
  from the same `.proto`, so the key is exact.
- **AsyncAPI / schema registry** — events and queues. Publisher and subscriber
  join on a channel or topic.

Shape: each repo's index links its own symbols to *contract operations* rather
than to each other. A contract operation becomes a third kind of node —
provided by one service's handler, consumed by another's client — and
`impact` traverses through it.

### Hard problems, stated not solved

- **Where do contract nodes live?** Contract operations are shared, so a
  per-repo DB is the wrong home for the operation identity, but the registry is
  metadata-only by design (steps 1-3). This probably needs a third store, or a
  deliberate exception to the metadata-only rule. **This is the central design
  question of step 5 and it is unresolved.** `[NEEDS REVIEW]`
- **Linking a handler to an operation** is framework-specific: a decorator, an
  annotation, a router registration, a generated stub. Realistically this needs
  per-framework support, starting with one or two.
- **Linking a client call to an operation** is harder than the handler side.
  Generated clients are tractable (the stub is a real symbol). Hand-rolled
  `fetch("/v1/invoices/" + id)` needs string analysis, which is exactly the kind
  of heuristic that produces the false edges the resolver already fights.
- **Version skew.** Two services can be built against different revisions of one
  contract. An edge asserting they connect may be asserting something false.
- **Trust and provenance.** Edges through a contract are inferred differently
  from LSP-resolved edges and must be tagged as such via `EdgeProvenance`, so a
  consumer can tell a compiler-verified edge from a spec-inferred one.

### Why it is worth the cost anyway

It is the only step here that answers a question no per-repo tool can answer at
all. Steps 4, 6, and 7 make existing capabilities wider; step 5 adds a new one.

Suggested scoping: **one protocol, one framework, one direction, behind a
flag** — most likely protobuf (exact keys, generated stubs on both sides)
before OpenAPI. Prove the model on the easy case before touching string-built
URLs.

## Step 6 — federated semantic search

### Blockers, all real

`hybrid_search` binds to one `Database`
(`crates/cartog-rag/src/search.rs`). Fanning out means N calls plus a merge, and
three properties make that merge ill-defined:

1. **RRF ranks are per-result-set.** Reciprocal-rank fusion produces a rank
   *within* a retrieval. Rank 1 of a 200-symbol service and rank 1 of a
   50k-symbol service are not the same quantity.
2. **In-degree centrality is per-graph** — the same issue as step 4, but here it
   feeds a score rather than a tie-break, so it cannot be sidestepped by
   grouping.
3. **Embedding fingerprints are per-DB.** `embedding_provider`,
   `embedding_model`, and `embedding_dimension` live in each database's
   `metadata`. Two projects embedded with different providers hold vectors in
   **different spaces**; comparing their cosine similarities is meaningless, and
   a dimension mismatch is not even mechanically comparable.

Blocker 3 is a hard precondition, not a ranking nicety: federation must verify
fingerprint identity across all N and refuse (or degrade to keyword-only) on
mismatch.

### The cheap check that might delete this step

Before any design work: **read every registered project's embedding
fingerprint and see whether they match.** If a real fleet embeds
inconsistently, federated semantic search is unavailable until a uniform
re-embed, and the step is blocked by an operational problem rather than a
design one.

That check is a few lines against the registry and should be surfaced in
`cartog projects list` as a diagnostic regardless of whether step 6 proceeds.

### If it proceeds

Any normalization must be proven, not argued, against the RAG relevancy
benchmark (`make bench-rag`) with a cross-project scenario added. The bar: a
federated query over N projects should not rank worse than the same query run
against the single correct project. Per the measure-before-and-after rule, a
claim of "good enough merging" without those numbers is not done.

### Rerank-the-union: likely the actual answer

The reranker changes the picture, and its signature settles the point rather
than merely suggesting it:

```rust
// crates/cartog-rag/src/provider.rs
fn score_batch(&mut self, query: &str, documents: &[&str]) -> Result<Vec<f32>>;
```

A cross-encoder takes **one query and a list of document texts**. It never sees
a `Database`, a rank, or an `in_degree`. Its scores are therefore a function of
`(query, document text)` alone — **absolute, and independent of which project a
document came from**.

So reranking a merged candidate pool is well-defined even though the retrieval
scores that produced the pool are not. That makes the pipeline for step 6:

1. Retrieve candidates per project (each DB's own FTS5 + vector + RRF).
2. Merge the candidate **texts** into one pool, discarding the per-project
   ranks entirely — they are the incomparable quantity, so do not carry them.
3. `score_batch` the union against the query; sort by that score.

This sidesteps blockers 1 and 2 completely rather than normalizing around them.
Blocker 3 (embedding fingerprint) still binds the *retrieval* stage, since each
project's vector search must be internally valid — but a fingerprint mismatch
now degrades to "that project contributes keyword-only candidates" instead of
poisoning the merged ranking.

Two constraints on this design:

- **`rerank_max` is a budget** (default 50, `rerank_min` 8, in `SearchTuning`).
  The union pool across N projects must be capped *before* scoring, so
  per-project retrieval limits have to shrink as N grows rather than each
  contributing a full candidate set.
- **Rerank scores are raw logits** — whole result sets can be negative — so they
  must never be scaled multiplicatively, blended as probabilities, or compared
  against an RRF score.

Remaining unknown is quality, not correctness: whether reranking a
cross-project union ranks as well as reranking a single project's pool. That is
a benchmark question (`make bench-rag`), not a design one.

Caveat: reranker scores are **raw logits** (whole result sets can be negative),
so they must never be scaled multiplicatively or blended as if they were
probabilities.

## Step 7 — shared read-only multi-project server

### The measurement already exists, and it argues for this step

`benchmarks/idle_memory.sh` documents real numbers: **four idle serve peers
measured 2.06 GB combined**, and RSS reported only 5-35 MB each because the
pages were compressed out — which is why the harness uses macOS `footprint` /
Linux `smaps_rollup` instead. The guard is 150 MB per idle process, against
measured baselines of 246 MB (eager reranker), 91 MB (lazy), 84 MB (no
reranker configured).

Correcting my own earlier framing: I said this step was gated on measuring
N × idle footprint. That measurement is largely already done, and the
per-process floor is ~84-91 MB even with the reranker lazy. At
5 projects × 2 clients the duplication is real. The remaining unknown is not
the footprint but whether users actually run that many at once — which the
registry (step 1) answers.

### Design: read-only by construction

Every costly and dangerous part of a multi-project server is on the **write**
path: single-writer election, the watcher, LSP child processes, the consent
gate, the degraded state. Every valuable part is read fan-out. So the opt-in
server is read-only by construction:

- Opens projects via `Database::open_readonly`, **bounded by an LRU** (see
  below)
- Takes **no** serve or watch locks
- Runs **no** watcher, spawns **no** LSP
- Exposes the read tools with an optional `project` argument; the two write
  tools are **absent, not gated**
- Indexing stays where it works: each project's own `serve --watch` or
  `cartog watch`

#### Residency must be bounded: pinned default + LRU

An earlier draft said only "opens each project read-only", with no bound on how
many stay resident. That is a defect at any real fleet size, and there is a
shipped design to follow: graphify's MCP server keeps **one pinned default
context plus an LRU of project contexts**, capped at 8 by default and
overridable via a `GRAPHIFY_MAX_CONTEXTS` env var (invalid values fall back to
the default; zero and negatives clamp to 1). The pinned default is excluded
from that limit. Each entry is keyed on the graph file's `(mtime_ns, size)`, so
a changed file invalidates its own entry.

Three properties worth copying verbatim:

1. **Pin the default, LRU the rest.** The project the session is actually
   working in stays warm without consuming a cache slot, so a burst of routing
   to sibling projects cannot evict the one the agent keeps coming back to.
   For cartog the pinned entry is the cwd-resolved project — the same one a
   plain `cartog serve` would have served.
2. **Key the entry on file identity, not just path.** `(mtime_ns, size)` means
   a project re-indexed by its own `serve --watch` is picked up on next access
   rather than served stale from cache — which matters more here than for
   graphify, since cartog's per-repo DBs are updated by live peers.

   **But `(mtime_ns, size)` is not sufficient for cartog**, and this is the one
   place the design must diverge rather than copy. graphify reads a `graph.json`
   that is rewritten wholesale, so the file's mtime and size move with its
   content. A SQLite DB in WAL mode does not: a writer can commit into the
   `-wal` sidecar leaving the main file's mtime and size unchanged, so a cached
   entry would serve stale data with no signal. Key on SQLite's own
   `PRAGMA data_version` instead — it changes whenever another connection
   commits — or on the `-wal` file's identity alongside the main file's.
   Reusing the JSON-shaped key here would be a correctness bug, not an
   optimization.
3. **A bad project is a tool error, not a process exit.** graphify explicitly
   converts a load failure on a client-supplied path into a tool error so the
   shared server keeps serving other graphs. That is the same per-project
   degradation this document requires, and it is the failure mode a shared
   server most needs to get right.

Bounding matters *more* for cartog than for graphify: the per-process idle
floor is ~84-91 MB even with the reranker lazy (see
[the measurement](#the-measurement-already-exists-and-it-argues-for-this-step)),
and each resident DB adds page cache and prepared statements on top. An
unbounded shared server would reproduce, inside one process, exactly the
memory problem it exists to solve.

This removes the N-slot election problem entirely (so no TLA+/Loom work on the
verified single-writer protocol), the cross-project watcher, the consent matrix,
and LSP entanglement — while keeping the one real benefit, a single shared model
set.

`cartog serve` with no project list stays **exactly** as it is today. That is
the verified path and it remains the default.

### What does not go away

- **Crash blast radius.** A panic in one project's query path takes down the
  session for all of them. Per-project serve has total isolation; this trades
  that for memory. An honest doc says so.
- **One binary version must match all N DBs.** `open_readonly` rejects any
  schema mismatch, so after a cartog upgrade every project needs re-indexing
  before the shared server can read it. Must be per-project degradation
  (project E unavailable, A-D still serve), never a startup failure.
- **LSP must stay per-project.** Sharing a language server across roots is how
  you get wrong-root resolution on purpose, since LSP correctness depends on
  `rootUri`/`workspaceFolders` matching the project. A shared server spawning no
  LSP at all is the design; do not "optimize" this later.

### The serve/watch lock topology (corrected)

An earlier draft of this document claimed `serve --watch` takes only the
`serve-<hash>` slot, leaving it able to run concurrently with a standalone
`cartog watch` on the same DB. **That was wrong, and the code is careful here.**

`run_server` derives the watch slot from its own serve slot via
`serve_to_watch_slot` and hands it to the watcher it spawns
(`crates/cartog-mcp/src/single_writer.rs`). The code comment states the intent
directly: *"Claim the watcher's PID slot so a separately-running `cartog watch`
from a terminal correctly refuses to start against the same DB."*

So the real topology per DB is **two slots, both claimed by whoever is writing**:

| Actor | `serve-<hash>` | `watch-<hash>` |
|---|---|---|
| `cartog serve` (no `--watch`) | primary, or read-only attach | not claimed |
| `cartog serve --watch` (primary) | held | held by its watcher |
| `cartog serve --watch` (read-only peer) | not held | not claimed — no watcher runs |
| `cartog watch` (standalone) | not claimed | held, hard-bails if taken |
| `cartog pull` | held for the swap | held for the swap |

An off-pattern serve slot is a **hard error** in `serve_to_watch_slot` rather
than a silent fallback to the global `watch.pid`, precisely so two embedders
cannot collide. The invariant that follows: **at most one writer per DB, and it
holds both slots.** There is no serve-vs-watch double-writer gap to close.

This matters for everything below: the concurrency model is already sound
per-DB, so the multi-project question is purely about *how many processes* and
*how much memory*, not about correctness.

## MCP process scenarios: mono-repo, multi-repo, fleet

The concern this section answers: *how do we avoid a machine full of processes,
duplicated models, and master/slave contention?* Each scenario below states the
process count, the memory, and the failure modes, using measured numbers where
they exist.

Baselines used throughout (from `benchmarks/idle_memory.sh`, measured on a
12-core arm64 host):

| Idle `serve` state | Footprint |
|---|---|
| reranker eagerly loaded | 246 MB |
| reranker lazy (**current default**) | 91 MB |
| no reranker configured | 84 MB |
| CI guard ceiling | 150 MB |

`footprint`/`smaps_rollup` numbers, not RSS — the script header notes four idle
peers measured **2.06 GB combined** while `ps` RSS showed 5-35 MB each, because
the pages were compressed out. Any capacity reasoning from RSS is wrong.

### The two invariants that make all of this safe

Established by reading the code, and they hold in every scenario below:

1. **At most one writer per DB, holding both slots.** `serve --watch` claims
   `serve-<hash>` *and* (via its watcher) `watch-<hash>`. Extra peers on the
   same DB lose the election and attach **read-only**, with the 2 write tools
   gated. A standalone `cartog watch` hard-bails rather than becoming a second
   writer. See
   [The serve/watch lock topology](#the-servewatch-lock-topology-corrected).
2. **Election is per-DB, and locks are never held across a wait.** The lock is
   an `O_EXCL` PID file, acquired once at startup and released on `Drop`. There
   is no lock ordering to get wrong, because a process only ever contends for
   the two slots of the DB it was pointed at — and `pull`, the one actor that
   takes both explicitly, takes them in a fixed order and refuses rather than
   blocking. **Deadlock is structurally absent: no actor ever waits while
   holding.**

The remaining risk is therefore **not** deadlock or split-brain. It is process
count and resident memory. That is a capacity problem, and it has an engineering
answer.

### Scenario A — mono-repo, one editor (today's default)

One `serve --watch`. Holds both slots, runs the watcher, owns the index.

- **Processes:** 1 (+ N transient LSP children during an index pass)
- **Memory:** ~91 MB idle
- **Verdict:** the verified path. Nothing in these specs changes it.

### Scenario B — mono-repo, several editors on one repo

Three editors, same repo: 3 `serve` processes, 1 primary + 2 read-only
attachers. Each attacher runs a promoter polling every **10 s**
(`DEFAULT_PROMOTER_POLL_INTERVAL`).

- **Processes:** 3
- **Memory:** ~273 MB, and **the two attachers pay for a model set they will
  never use** unless a semantic query arrives
- **Failure mode:** none for correctness. If the primary dies, one attacher
  promotes within ~10 s and starts a watcher.
- **Cost that matters:** duplicated idle footprint, not contention.

Note the promoters are cheap but not free: each is a filesystem `stat` every
10 s. At 3 peers that is invisible; the point is it scales linearly with peer
count, so it belongs in the capacity arithmetic below.

### Scenario C — monorepo indexed as one root

One repo, many packages, **one** DB and one `serve`. Same process/memory profile
as Scenario A.

The cost here is not concurrency — it is resolution quality. Measured on a
real React+Vue monorepo (2,119 files, 16,168 edges): **29 % of edges resolved
heuristics-only, 58 % with LSP working**, versus **88 %** for one app indexed
standalone. The cross-app noise came from the `unique_global` tier matching
unrelated symbols across packages.

So a monorepo has a genuine choice, and it is a **quality-vs-process-count**
trade, not a concurrency one:

| Approach | Processes | Resolution | Notes |
|---|---|---|---|
| One root, one DB | 1 | ~58 % (LSP) | cross-package noise; simplest |
| Per-package DBs via `--db` | 1 per package you serve | ~88 % | N× the idle memory if all served at once |

Neither is a concurrency hazard — separate DBs mean separate slots, so there is
no contention between them. The per-package split trades memory for accuracy.
A package-boundary resolution tier (out of scope here) would give the accuracy
without the process multiplication, which is why it is the better long-term fix.

### Scenario D — multi-repo fleet, per-project servers (the one to worry about)

This is where the user's concern is well founded. Process count is
**projects × editors**, and each process pays a full idle footprint.

| Repos | Editors | Processes | Idle memory (91 MB each) |
|---|---|---|---|
| 3 | 1 | 3 | ~273 MB |
| 5 | 2 | 10 | ~910 MB |
| 10 | 2 | 20 | ~1.8 GB |
| 20 | 3 | 60 | ~5.5 GB |

**Still no deadlock and no split-brain** — every DB has its own independent
election. But 60 processes holding 5.5 GB to serve mostly-idle indexes is a
machine-usability problem, exactly as feared.

Three mitigations, in order of cost:

1. **Lazy models are already the default.** The reranker is not built until
   first use (`lazy_reranker`, `LazyReranker`), which is what keeps idle at
   91 MB instead of 246 MB. Preserve this: an eager load would nearly **triple**
   every row of the table above. The `reranker_is_loaded` assertions and
   `make bench-memory` exist to keep it true.
2. **Do not serve what you are not using.** The multiplier is *editors*, and
   most editors open a repo on demand. Wiring `serve` for every project in every
   client is the pathological case; wiring it per active workspace is the normal
   one.
3. **The shared read-only server (step 7)** collapses the *projects* dimension
   from N processes to 1, sharing one model set. This is its real justification,
   and the numbers above are the argument.

### Scenario E — shared read-only server (step 7)

One process, N project DBs, LRU-bounded residency, **no locks, no watcher, no
LSP**, write tools absent.

| Repos | Editors | Processes | Idle memory |
|---|---|---|---|
| 20 | 3 | 1 shared + 3 per-repo writers | ~91 MB + 3×91 MB ≈ 364 MB |

Versus 5.5 GB in Scenario D — a **15× reduction** at 20 repos.

Why it is concurrency-safe by construction:

- **Takes no locks**, so it cannot contend with, block, or be blocked by any
  writer. It is invisible to the election protocol.
- **Cannot be a master or a slave.** There is no promotion, no primary/secondary
  relationship, nothing to arbitrate. The master/slave concern does not apply
  because a read-only server never participates.
- **Reads under WAL**, which lets readers and writers proceed concurrently; the
  `busy_timeout` bounds the one case (a long read against a checkpointing
  writer) where they interact.

What it costs, stated plainly:

- **Crash blast radius:** one panic ends the session for all N projects. Per-repo
  serve isolates totally. This is the trade, and it is not removable.
- **Version coupling:** one binary must match all N schemas, so a cartog upgrade
  makes every not-yet-re-indexed project unreadable until re-indexed.
- **Writes still need a per-repo writer.** The shared server does not index, so
  Scenario D's writer processes do not disappear — they are just no longer
  duplicated per editor.

### Scenario F — the pathological configuration to avoid

Worth naming so nobody builds it: **a shared server that also writes.** That
would mean one process holding N `serve-<hash>` locks and N `watch-<hash>`
locks, being primary for some DBs and read-only for others simultaneously, with
N promoters running inside it.

Why this is the design to refuse:

- The election protocol is TLA+-specified and Loom-checked for **one slot per
  process** (`specs/tla/`, `make loom`). N-slots-in-one-process is a different
  state space, not a configuration change — it would need new spec work before
  any code.
- It reintroduces the master/slave complexity the user is right to fear: a
  process that is simultaneously master of DB A and slave of DB B, with partial
  promotion states if it dies mid-takeover.
- It multiplies the crash blast radius by the write path, the riskiest part.

**Rule to record: any multi-project server is read-only. Writing stays
one-process-per-DB, always.** That single constraint is what keeps the
concurrency model as simple as it is today.

### Capacity summary

| Scenario | Processes | Idle memory | Deadlock risk | Split-brain risk |
|---|---|---|---|---|
| A: mono, 1 editor | 1 | ~91 MB | none | none |
| B: mono, 3 editors | 3 | ~273 MB | none | none (election) |
| C: monorepo, 1 root | 1 | ~91 MB | none | none |
| C′: monorepo, per-package | 1/package | 91 MB × pkgs | none | none |
| D: 20 repos × 3 editors | 60 | ~5.5 GB | none | none |
| E: shared read-only + writers | 1 + writers | ~364 MB | none | none |
| F: shared **writable** server | 1 | — | **unspecified** | **unspecified** |

Every row except F is deadlock-free and split-brain-free by construction. The
problem the specs must solve is row D's memory, and row E is the answer. Row F
is the thing to refuse.

### What to measure before building step 7

The tables above extrapolate from a single measured 4-peer figure. Before
committing:

1. Run `benchmarks/idle_memory.sh` with 10 and 20 concurrent peers to confirm
   per-process cost stays roughly linear rather than growing (shared page cache
   across DBs could make it sublinear; allocator behavior could make it worse).
2. Instrument how many projects a real user actually serves at once — the
   registry (step 1) provides this, and it decides whether row D is a real
   configuration or a hypothetical one.
3. Measure resident cost per *additional DB inside one process* for step 7,
   which the LRU cap must be sized against.

If step 2 shows users serve 2-3 projects, row D never materializes and step 7 is
unnecessary. That is the honest gate.

## Cross-cutting requirements

Applying to any of steps 4-7 that gets built:

- **Per-project degradation, never wholesale failure.** One stale, missing, or
  corrupt project must not break a federated answer. Every result carries which
  projects answered and which were skipped, with reasons.
- **Read-only toward other projects.** Discovery grants no write access. No step
  here may index, embed, or mutate another project's database.
- **Provenance survives federation.** Results keep their project id and their
  `EdgeProvenance`, so an agent can tell a compiler-verified edge from a
  spec-inferred one (step 5) and a local result from a remote one.
- **Untrusted content stays untrusted.** Descriptions, symbol names, and file
  paths from another project are repository-authored data: escaped on any
  rendering surface, parameterized in SQL, never treated as instructions.
- **Compact by default** on MCP surfaces, like the existing tools. Federated
  results multiply payload size by N, so the budget trimming must be applied to
  the merged set, not per project.

## Documentation surface

Whichever step lands first, per the docs + site-sync rule: this document and
[project-registry.md](project-registry.md) must both be listed in
`docs/explanation/README.md` **and** `docs/README.md` (both indexes enumerate
every explanation doc), and any new CLI flag, MCP tool, or config key updates
`docs/reference/`, `docs/usage.md`, README, `AGENTS.md`/`CLAUDE.md`,
`skills/cartog/SKILL.md`, and both `site/src/pages/{index,usage}.astro` — a new
config key needing **both** a summary-table row and an explanation + example
block.

## Limitations that survive every step

Even with all four steps built, these remain true. They are the shape of the
problem, not gaps in the plan:

- **A federated answer is only as fresh as the least-recently-indexed project.**
  Fan-out reads live DBs, but a repo nobody has indexed for a month contributes
  month-old symbols with no visible marker in a merged result set. Any federated
  output must carry per-project `last_indexed`, or it silently mixes eras.
- **One binary version must match every project it reads.** `open_readonly`
  refuses on any schema mismatch, so a cartog upgrade makes every not-yet-
  re-indexed project unreadable to a shared server or a federated query. This is
  the single biggest operational cost of steps 4 and 7, and it grows with fleet
  size: 20 repos means 20 re-indexes before cross-project queries work again.
- **Cross-service edges will never be complete.** Contracts cover what is
  declared. A hand-built URL, a dynamically-dispatched client, a message routed
  by a runtime-computed topic — all invisible. Step 5 raises coverage from zero;
  it does not reach the certainty an in-repo compiler-checked call graph has, and
  its edges must stay provenance-tagged so a consumer can weigh them differently.
- **Federated semantic search requires fleet-wide embedding uniformity** — same
  provider, same model, same dimension. That is an operational discipline, not a
  feature, and it breaks the moment one repo is embedded on a different machine
  with a different config.
- **A shared server trades isolation for memory, permanently.** No amount of
  design removes the fact that one process serving N projects fails for all N.
  The read-only-by-construction design shrinks the blast radius to query paths;
  it cannot eliminate it.
- **None of this makes cartog a workspace tool.** There is still one index per
  repo, one walk root, one config. Package-boundary resolution inside a monorepo
  is a *separate* problem from cross-repo queries, and steps 4-7 do not touch it.

## Recommended order

0. **P1 + P2** (see [Inherited prerequisites](#inherited-prerequisites)) — no
   cross-DB step is implementable without them. P1 is a move-only refactor that
   lands on its own.
1. ~~The `serve`/`watch` slot boundary decision~~ — **withdrawn.** The
   topology is already correct: `serve --watch` claims both slots. See
   [The serve/watch lock topology](#the-servewatch-lock-topology-corrected).
2. **The embedding-fingerprint diagnostic** in `cartog projects list` — a few
   lines, and it tells you whether step 6 is even reachable.
3. ~~**Step 4** (federated `search`)~~ — **shipped.** Grouped-per-project
   output, so the cross-graph ranking question never had to be answered.
4. **Step 5** (contract edges) — the differentiating feature; scope to one
   protocol behind a flag. Independent of 4, 6, 7.
5. **Step 6 / step 7** — only on evidence. Step 6 needs matching fingerprints
   plus a benchmark; step 7 needs users who actually run many projects at once.

## Entry conditions: what evidence unblocks each step

Steps 4-7 are gated "on evidence" throughout this document, which is only
actionable if the evidence is named and someone is collecting it. Phase 1 (the
registry), step 3 (descriptions) and step 3b (backfill) all shipped, so the
instruments now exist. This section states what each step is waiting for, and
what would falsify it.

**Nothing here is a commitment to build.** A step whose entry condition is never
met is a step that was correctly not built, and this section is as much a record
of how to decide *against* one.

### The instrument that has to run first

Every condition below is a fact about how people actually use the registry, and
none of it is knowable from the code. What is missing is not analysis but
observation: **routing has to be used in earnest before its failures mean
anything.** Concretely, that means using `cartog_list_projects` + `--db` as the
normal way to answer a cross-repo question for long enough to accumulate cases
where it was awkward, and keeping the cases rather than the impression.

Until then, every step below is a hypothesis about a problem nobody has
demonstrated having. The registry cost ~11ms per changed pass to build; the
next step should not cost more than that on the strength of a guess.

### Step 4 — federated exact-symbol search — **built**

Built without waiting for its entry condition, on an explicit instruction to
implement it. Recorded plainly because the reasoning still stands: step 4 was
the cheapest of the four and the only one whose value does not depend on the
others, and it was also the one most likely to be *unnecessary* — descriptions
exist so the project is usually known before the query, and step 4's premise is
that they sometimes are not enough.

**The falsification test therefore still applies, after the fact:** if in
practice the project is nearly always known before the search, `--all` will go
unused and that is the answer. What to watch is simply whether it gets reached
for. It cost no schema change and no new crate, so an unused `--all` is a small
loss; the note is here so a future reader does not mistake "shipped" for
"validated".

### Step 5 — cross-service contract edges

| | |
|---|---|
| **Entry condition** | Concrete cases of "who calls this endpoint?" that the per-project graph cannot answer, in a fleet where the contract is actually declared somewhere machine-readable (OpenAPI, protobuf, a schema registry). |
| **Evidence to keep** | The contract artifact, and whether producer and consumer both reference it in a form a parser could join on. |
| **Falsified if** | The join key does not exist in practice — contracts are prose, hand-rolled clients, or drifted from the code. Then the edges would be derived from something untrue, which is worse than absent. |
| **Blocked on** | [Open question 2](#open-questions) (where contract operations live), which this document calls step 5's central unresolved question. |

This is the differentiating feature and the one whose entry condition is
hardest to satisfy honestly. The failure mode is not "we built it and it was
slow" but "we built it and it was confidently wrong", which is why the
falsification test is about the join key existing rather than about demand.

### Step 6 — federated semantic search

| | |
|---|---|
| **Entry condition** | Two things, both mechanical: the `embed-mismatch` marker is *absent* across the projects in question (matching provider/model/dimension — vectors in different spaces cannot be merged meaningfully), **and** rerank-the-union beats per-project search on `make bench-rag`. |
| **Evidence to keep** | The marker distribution across a real fleet, then the benchmark delta. |
| **Falsified if** | Fleets are heterogeneously embedded in practice, or the union does not beat the parts. The benchmark is the arbiter, not the argument. |
| **Blocked on** | The marker survey, which needs no new code — `cartog projects list` already reports it. |

The [cheap check that might delete this step](#the-cheap-check-that-might-delete-this-step)
is the right first move here, and it is now runnable: backfill (step 3b) is what
makes a *whole fleet's* markers visible without re-indexing each project, which
is exactly the survey this step needs.

### Step 7 — shared read-only multi-project server

| | |
|---|---|
| **Entry condition** | Evidence that several projects are genuinely served *at once*, often enough that their combined idle footprint matters. [Open question 4](#open-questions) names the registry as the instrument. |
| **Evidence to keep** | Concurrent `live` marker counts over time, and the memory that actually costs. |
| **Falsified if** | The realistic concurrent count is two or three. `make bench-memory` already guards the idle footprint, so a handful of peers is a solved problem and step 7 is a large change buying nothing. |
| **Blocked on** | The observation itself. `live` markers make it countable; nothing samples them over time yet. |

Note the asymmetry: step 7 is the largest change of the four and has the
weakest entry condition, because "how many projects does one person serve
concurrently" is a question about people, not code. It should stay last.

### Why this is not scheduled

Each condition needs elapsed time using the shipped feature, not a work item.
The registry, its descriptions and its backfill are the instruments; the next
decision point is whichever condition trips first, and the honest answer today
is that none has.

## Open questions

1. **Is a flat merged ranking wanted at all**, or is grouped-by-project the
   right presentation for an agent? Decides step 4's hardest sub-problem. Note
   this is now partly answered by construction: since `rank` is coarse and
   `in_degree` dominates most orderings, a flat merge *cannot* be built
   defensibly without a ranking benchmark, while grouped output needs none.
2. **Where do contract operations live** if the registry stays metadata-only?
   The central unresolved question of step 5.
3. ~~Does rerank-the-union sidestep RRF normalization entirely?~~
   **Resolved** — yes, on the retrieval-score side. `RerankerProvider::score_batch`
   takes `(query, documents)` with no DB coupling, so its scores are absolute.
   See [Rerank-the-union](#rerank-the-union-likely-the-actual-answer). What
   remains is a *quality* question for `make bench-rag`, not a design one.
4. **How many projects does a real user serve concurrently?** The registry
   answers this, and it is the gate on step 7.
5. **Should federated search be a distinct tool** (`cartog_search_all`) rather
   than a flag on `cartog_search`? **Leaning distinct tool.** Each MCP tool
   carries its own `#[tool(description = ...)]`
   (`crates/cartog-mcp/src/tools/search.rs`), and that description is the only
   thing steering an agent's choice. A boolean flag on `cartog_search` is
   invisible in tool selection — the agent must already know to set it — whereas
   a separate tool can say *when* cross-project search is appropriate. The cost
   is one more tool in a 16-tool surface (17 with `cartog_list_projects`). Worth
   settling with a skill eval (`make eval-skill`) rather than by argument, since
   routing quality is exactly what that harness measures.
