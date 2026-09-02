# cartog-registry

User-global state for [cartog](https://github.com/jrollin/cartog): the per-user
state directory, PID-lock slot derivation, and live-peer detection.

Part of the [cartog](https://crates.io/crates/cartog) workspace. Not intended
as a standalone library.

## What it owns

Everything cartog keeps **per user** rather than per project. The graph crates
(`cartog-db`, `cartog-indexer`, …) are all per-project; this crate is the one
place that knows about the machine as a whole.

- **State directory resolution** — the XDG-compliant per-user directory that
  hosts `state.toml` and the PID lock files written by `cartog serve` /
  `cartog watch`:

```text
Linux:   $XDG_STATE_HOME/cartog/     (typically ~/.local/state/cartog/)
macOS:   ~/Library/Application Support/cartog/
Windows: %LOCALAPPDATA%\cartog\
```

- **Slot derivation** — `slot_for_db("serve", db_path)` reduces a database
  path to a stable, filesystem-safe `serve-<16 hex chars>` name. Two peers
  reaching the same physical database by different paths (relative vs
  absolute, through a symlink, macOS `/tmp` → `/private/tmp`) agree on the
  slot, which is what makes cartog's single-writer election correct. The
  generator lives next to nothing else that could drift from it: the slot it
  produces is the slot `cartog-process-lock` writes as `<slot>.pid`.

- **Live-peer detection** — `detect_live_serve_peer` finds a running
  `cartog serve` holding a given database's serve lock. Used by
  `cartog index` to defer its LSP pass to a peer's warm language servers.

- **The project registry** — `projects.sqlite` in the state directory, one row
  per indexed project. It lets a session in one repository discover the *other*
  indexed projects on the machine — where their databases live, and a summary
  of what each holds:

```text
record_project(&facts)     record a project after an index pass commits
list_projects(schema_ver)  every registered project, with staleness markers
forget_project_at(..)      drop one row; never touches the project's index
prune_projects_at(..)      drop rows whose database file is gone
```

  It stores paths, counts, languages and the embedding fingerprint — never any
  code. `CARTOG_REGISTRY` relocates the file (absolute paths only); set it to
  an empty value to disable reads and writes entirely.

  A registry write never fails its caller: registration is bookkeeping riding
  on an index pass, so an unwritable state directory, a contended file or a
  corrupt registry costs the caller a log line and nothing else.

## Why it is a separate crate

These helpers began in the `cartog` binary crate, which made them unreachable
from `cartog-mcp` and `cartog-watch` — the crates that *derive* the slots they
lock on, since the binary depends on them and not the reverse. Both crates
documented the binary's function as the way to build a slot while being unable
to call it. Extracting them here makes that reference real.

The design rationale, and the later phases built on top of this one (project
descriptions, federated queries), are in
[docs/explanation/project-registry.md](https://github.com/jrollin/cartog/blob/main/docs/explanation/project-registry.md).

## License

MIT
