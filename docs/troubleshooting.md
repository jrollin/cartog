# Troubleshooting

A living list of issues that turn up on first-run or after upgrades.
If something here is out of date or you hit a new problem,
[open an issue](https://github.com/jrollin/cartog/issues).

## Installation

### `cargo install cartog` is slow the first time

Tree-sitter grammars are C code compiled at `opt-level = 1`, which is slower
than `opt-level = 0` but produces usable dev-mode parsers. A release build
takes 2–4 min on a warm machine. Subsequent `cargo install` calls reuse the
build cache.

### I got a pre-built binary but `cartog rag setup` fails

Pre-built binaries ship without the `lsp` feature on some platforms. If you
want LSP-based edge resolution (required to hit the 97% recall number in the
README), build from source with
`cargo install cartog --features lsp`.

## First index

### `cartog index .` appears to hang

Indexing a 50k-LOC repo cold takes a few seconds, sometimes longer if
tree-sitter is compiling on the first invocation. The CLI shows a spinner on
stderr when attached to a TTY. If you still see nothing after 60s, re-run
with `RUST_LOG=info cartog index .` and open an issue with the output.

### "no LSP server found on PATH" during `cartog index`

Cartog auto-detects language servers and uses them to boost edge resolution.
If none are installed it silently falls back to heuristics. The install hints
for supported servers are printed when the feature is on but no server is
available. To silence the check entirely, pass `--no-lsp`.

### LSP server looks stuck

Cartog waits up to 20 s for the server to load its project model (overridable
via `CARTOG_LSP_READY_TIMEOUT_SECS`). The server's own stderr is piped to
`<tmp>/cartog-lsp/<binary>.log` — check there first for a real error.

## Re-indexing

### I upgraded cartog and my index was cleared

Destructive schema migrations (e.g. the 2→3 stable-id bump) rebuild the index
from scratch. Cartog first creates a `VACUUM INTO <db>.pre-v<old>-<ts>.bak`
copy of the old database so you can roll back by pointing `--db` at the
backup. Run `cartog index .` once to rebuild against the new schema.

### `cartog watch` triggers repeated re-indexes during `git pull`

The default debounce is 5 s, which should collapse most bulk operations into
one re-index. If you still see bursts, raise it via
`cartog watch --debounce 30` or bump the default in your project's
`.cartog.toml`.

### RAG embeddings are stale after I changed `.cartog.toml`

Changing `embedding.provider`, `embedding.model`, or `embedding.dimension`
invalidates stored embeddings. Run
`cartog rag index . --force` to re-embed from scratch.

## Configuration

### `.cartog.toml` isn't picked up

Cartog walks up from the current directory looking for `.cartog.toml`,
stopping at the git root. If your tree has no `.git`, put the config in the
cwd or pass `--db` explicitly. Run `cartog config` to see the resolved
config path and database path, or
`RUST_LOG=info cartog <cmd>` for a log line on every invocation.

### Multiple `.cartog.toml` in nested projects

Only the nearest one is used. There is no merging.

### What goes in `.cartog.toml`?

See [`.cartog.toml.example`](../.cartog.toml.example) at the repo root for a
fully commented template.

## Queries

### `cartog refs X` returns fewer hits than I expect

Check whether the `lsp` feature is compiled in:
`cartog doctor` shows "LSP: available" when it is. Heuristic-only resolution
hovers around 25–37 % across languages; LSP-backed resolution is 44–81 %
depending on language.

### `cartog impact X --depth 5` feels slow on the first call

The recursive CTE is fast, but SQLite may still need to populate the page
cache. A second call should drop back to sub-ms. If it doesn't, please attach
the output of `cartog stats` to your issue.

## Reporting bugs

A useful issue includes:

- `cartog --version`
- `cartog doctor`
- `cartog stats`
- The failing command, run with `RUST_LOG=debug`.
