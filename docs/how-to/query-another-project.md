# How to Query Another Project on This Machine

> For the command reference see [../reference/cli.md](../reference/cli.md#cartog-projects-listforgetprune);
> for `CARTOG_REGISTRY` see [../reference/config.md](../reference/config.md#environment-variables);
> for why the registry stores metadata rather than merging graphs see
> [../explanation/project-registry.md](../explanation/project-registry.md).

cartog indexes one project per database. When a question spans two repositories — a sibling
service, a shared library — you do not need a merged index: you need the *other* project's
database path, then any ordinary cartog command with `--db`.

## The two steps

```bash
cartog projects list                 # 1. find the other project
cartog search CreateShipment --db /path/to/other/.cartog/db.sqlite   # 2. query it
```

Every `cartog index`, `cartog rag index`, `cartog pull`, watcher re-index, and `cartog serve`
startup records its project, so anything you have indexed on this machine is already listed.
There is nothing to enable.

## Finding the path programmatically

`--json` gives a stable shape for scripts and agents:

```bash
DB=$(cartog projects list --json | jq -r '.projects[] | select(.name=="svc-shipping").db_path')
cartog search CreateShipment --db "$DB"
cartog outline internal/ship/ship.go --db "$DB"
cartog impact CreateShipment --db "$DB"
```

Any command that accepts `--db` works this way — `search`, `outline`, `refs`, `callees`,
`impact`, `hierarchy`, `deps`, `trace`, `context`, `map`, `stats`, `rag search`.

## From an agent (MCP)

The `cartog_list_projects` tool returns the same rows. An agent asked about a different
repository should call it, take the `db_path`, and shell out with `--db` rather than guessing
paths or reading files. `current: true` marks the project the server is already serving — use
the normal tools for that one.

## Reading the markers

```
svc-billing        8134 symbols   412 files  rust, markdown    2h ago  [live]
  /home/u/work/svc-billing/.cartog/db.sqlite
svc-shipping       2201 symbols    98 files  go                3d ago  [stale-schema v6]
  /home/u/work/svc-shipping/.cartog/db.sqlite
old-thing             ? symbols        ?     —                 never   [missing]
  /home/u/work/old-thing/.cartog/db.sqlite
```

| Marker | What to do |
|--------|-----------|
| `live` | A `cartog serve`/`watch` peer holds it. Querying with `--db` is still fine (readers do not block). |
| `stale-schema vN` | Written by an older cartog. Run `cartog index` in that project before querying it. |
| `missing` | The database is gone. `cartog projects prune` drops the row. |
| `embed-mismatch` | Its embeddings use a different provider/model/dimension, so `rag search` scores are not comparable with your other projects. Re-run `cartog rag index` there to align. |

A `?` count means "not known", not zero — the registry caches whatever the writing command
happened to measure.

## Housekeeping

```bash
cartog projects prune --dry-run   # what would be dropped
cartog projects prune             # drop rows whose database is gone
cartog projects forget <target>   # drop one row; the index itself is untouched
```

`forget` accepts a project id, root path, database path, or name. A **name** is acted on only
when it identifies exactly one project: two workspaces each holding an `api` directory produce
two rows named `api`, so an ambiguous name drops nothing and prints the candidate ids.

Forgetting a project does not delete its index — a later `cartog index` there re-registers it.

## Turning the registry off

```bash
export CARTOG_REGISTRY=            # empty: disables both reads and writes
export CARTOG_REGISTRY=/shared/projects.sqlite   # or relocate it (absolute paths only)
```

With it disabled, `cartog projects list` reports that there is no registry rather than an
empty list, and no command records anything. A relative path is refused — it would give every
directory its own registry, which defeats the purpose.

## What this does not do

- **No cross-project edges.** A call from one repository into another is not an edge; each
  index is independent. Ask each project separately.
- **No searching every project at once.** One `--db` per command, by design: it keeps the
  cost of a listing independent of how many projects you have.
- **No description of what each project is for.** The routing signal is name, languages, and
  size. Treat a name match as a hint.
