# Troubleshooting

A living list of issues that turn up on first-run or after upgrades.
If something here is out of date or you hit a new problem,
[open an issue](https://github.com/jrollin/cartog/issues).

> Related docs: [usage.md](usage.md) for the full CLI reference, [editor-integration.md](editor-integration.md) for editor-specific setup.

## Installation

### `cargo install cartog` is slow the first time

Tree-sitter grammars are C code compiled at `opt-level = 1`, which is slower
than `opt-level = 0` but produces usable dev-mode parsers. A release build
takes 2–4 min on a warm machine. Subsequent `cargo install` calls reuse the
build cache.

### LSP-based edge resolution isn't kicking in

LSP is enabled by default. If you installed with `--no-default-features`, the
`cartog-lsp` dependency was omitted — reinstall with plain `cargo install
cartog`. Otherwise, confirm a language server is on `PATH` (e.g.
`rust-analyzer`, `pyright`, `typescript-language-server`, `gopls`,
`intelephense`, `dart`, `sourcekit-lsp`, `kotlin-language-server`) and that you
haven't passed `--no-lsp` at runtime.

For PHP, cartog probes `intelephense` first and falls back to `phpactor`.

For Dart, cartog invokes `dart language-server --protocol=lsp` from the Dart
SDK; install the SDK from <https://dart.dev/get-dart>.

For Swift, cartog invokes `sourcekit-lsp`, which ships with the Swift toolchain
and Xcode.

For Kotlin, cartog invokes `kotlin-language-server`; install it from
<https://github.com/fwcd/kotlin-language-server> (it does not ship on a common
PATH by default).

Intelephense is closed-source with an optional premium tier; the free tier
handles `textDocument/definition` used by cartog.

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
invalidates stored embeddings. cartog now records the provider + model + dim
as a fingerprint in the DB and **auto-wipes** `symbol_vec` on the next open
when any field differs. Just run `cartog rag index .` to re-embed (no
`--force` needed). Older DBs that predate the fingerprint get a one-time
backfill without wiping.

## MCP server

### `MCP error -32000: Connection closed` when opening a second Claude Code window

This was the pre-Phase-2 symptom of two cartog processes racing on the
embedding-dimension migration. As of v0.17 cartog uses single-writer
election: the first `cartog serve` is the primary, the second attaches
read-only and exposes 11 of 13 MCP tools (`cartog_index` and
`cartog_rag_index` return a clear refusal pointing at the primary). If the
primary process dies, the secondary takes over within ~10s.

If you still see the error, check that the cartog binary is at v0.17 or
newer (`cartog --version`) and that you haven't set `CARTOG_SINGLE_WRITER=0`.

### `cartog watch` refuses to start with "another cartog process holds the watch lock"

Exactly one `cartog watch` may run per project at a time. The watcher
refuses to start (rather than attaching read-only — unlike `cartog serve`)
because a second watcher would re-index the same files redundantly.

Find the holder by listing `<state_dir>/watch-*.pid` (one file per
project). Each file's first line is the PID, second is the OS start
time. Stop the holder process. **Do not manually delete the lock file**
— cartog auto-cleans it on the next acquire once the recorded PID +
start_time no longer matches a live process, and deleting it under a
live writer would let a second watcher start and corrupt the index.

PID file names are DB-scoped: `<state_dir>/serve-<hash>.pid` and
`watch-<hash>.pid` where `<hash>` is a 16-char SHA-256 prefix of the
canonical DB path. Run `cartog config` to see your DB path, then list
the state dir to find the matching file.

### My cartog process exited but the PID file is still there

cartog unlinks its PID file via the `ProcessLock` Drop impl on clean exit
(rmcp shutdown, SIGINT, SIGTERM). A hard kill (`kill -9`, power loss)
leaves the file behind. The next `cartog serve` / `cartog watch`
startup runs a `sweep_stale_locks` pass that reaps every dead `.pid`
file in the state dir (not just the slot being claimed), so leftovers
from crashed peers disappear automatically — no manual action needed.

### MCP stderr is full of `[ERROR]` lines that look like info-level messages

Fixed in v0.17. When `cartog serve` runs as an MCP child (stderr not a
TTY), the default tracing level is now `warn` so info-level progress lines
no longer surface as `[ERROR]` in the parent's debug log. Set
`RUST_LOG=info` to opt back in.

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

## Database

### "database at `<path>` is corrupt or not a cartog database"

The SQLite file is truncated or not a cartog DB (a stray `.cartog/db.sqlite`,
an interrupted copy, etc.). Delete the named file and run `cartog index .` to
rebuild from scratch.

### "database at `<path>` is not writable"

The DB file or its directory is read-only. Fix the filesystem permissions, or
point `[database].path` (or `--db` / `CARTOG_DB`) at a writable location.

## RAG / embeddings

### "cannot reach Ollama at `<url>`"

The Ollama provider couldn't connect. Start the server (`ollama serve`) and
confirm `[embedding.ollama].base_url` in `.cartog.toml` matches it (default
`http://localhost:11434`).

### "Ollama has no model `<model>`"

The configured embedding model isn't pulled. Run `ollama pull <model>` (e.g.
`ollama pull nomic-embed-text`), then re-run `cartog rag index`.

### "this build was compiled with `--no-default-features`"

The Ollama provider ships in every default build (install.sh, GitHub Releases,
`cargo install cartog`). You only see this if you rebuilt with
`--no-default-features`. Rebuild with default features, or add
`--features ollama-embedding`.

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

### `cartog refs/callees/impact/hierarchy X` returns nothing

These commands match an **exact** symbol name. When there's no exact match but
similar names exist, cartog appends `— did you mean: A, B, C?`. Use one of the
suggestions, or run `cartog search X` (fuzzy: prefix + substring) to find the
exact name first. The MCP tools surface the same suggestion in their response.

## Logging and signals

### Info-level lines appear as `[ERROR]` in my MCP client debug log

The CLI uses `info` level when stderr is a TTY and `warn` otherwise. Under
`tmux`, `docker run -t`, or other terminal-emulating launchers, `IsTerminal`
may return true even though the MCP parent is consuming stderr, so info
lines get surfaced as `[ERROR]` by the client. Set `RUST_LOG=warn` to opt
out.

### What happens if cartog cannot install a SIGINT handler

`cartog serve` and `cartog watch` register a SIGINT handler at startup. If
the install fails (rare, usually a sandboxed runtime), the handler future
parks instead of resolving immediately. Other shutdown signals (SIGTERM,
service shutdown) still work; only Ctrl-C is unavailable until restart.

## Reporting bugs

A useful issue includes:

- `cartog --version`
- `cartog doctor`
- `cartog stats`
- The failing command, run with `RUST_LOG=debug`.
