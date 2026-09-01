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

## Why it is a separate crate

These helpers began in the `cartog` binary crate, which made them unreachable
from `cartog-mcp` and `cartog-watch` — the crates that *derive* the slots they
lock on, since the binary depends on them and not the reverse. Both crates
documented the binary's function as the way to build a slot while being unable
to call it. Extracting them here makes that reference real.

A machine-local registry of indexed projects (`projects.sqlite`), letting one
session discover the other indexed projects on the machine, is proposed on top
of this crate in
[docs/explanation/project-registry.md](https://github.com/jrollin/cartog/blob/main/docs/explanation/project-registry.md).
It is not implemented yet.

## License

MIT
