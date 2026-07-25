# How to Configure LSP Servers

> For the full `[lsp]` config key reference, see [../reference/config.md](../reference/config.md).

## LSP server overrides

By default cartog resolves a language's LSP server from `PATH` (e.g.
`rust-analyzer`, `gopls`). `[lsp.<lang>]` overrides that with an explicit
command — most usefully a Dockerized server, so cartog can resolve edges on a
host without the language's native toolchain installed:

```toml
[lsp.dart]
command = ["docker", "run", "--rm", "-i",
           "-v", "${ROOT}:${ROOT}", "-w", "${ROOT}", "cartog-lsp-dart:stable"]
```

- `command` is the full argv; `command[0]` is the executable (looked up on
  `PATH` or given as an absolute path), the rest are its arguments.
- `${ROOT}` in any element expands to the indexed project root (host-absolute).
- **Path mirroring is required.** cartog talks to the server over stdio using
  `file://` URIs built from the host path. A container must therefore see the
  repo at the *same* path — hence `-v ${ROOT}:${ROOT} -w ${ROOT}`. A container
  path that differs from the host path will make every definition resolve as
  "external" and is unsupported.
- The override only applies to the keyed cartog language (`dart`, `go`,
  `python`, ...); it must be a language cartog already supports.
- The server's stderr is logged to `${TMPDIR}/cartog-lsp/<language>.log`.
- cartog sends `processId: null` in the LSP `initialize` for an override server
  (a native server gets cartog's real PID). A container runs in its own PID
  namespace where cartog's host PID does not exist, so a real `processId` would
  trip the LSP parent-liveness check and make some servers (notably pyright and
  typescript-language-server) exit at startup. Handled automatically — no config
  needed. The override server is still reaped when cartog exits normally.

Run `cartog index --force <path>` after adding an override; the server is
spawned during the LSP edge-resolution pass.

Pinned Docker recipes for all 16 LSP languages live in `benchmarks/lsp-images/`
(`<lang>.Dockerfile` → `cartog-lsp-<lang>:stable`). Build them with
`make lsp-images`, then `resolution_rate.sh --docker-lsp` wires the override for
each (strict: a missing image is an error, not a host fallback). Each image
resolves identically to its host-installed server. See `benchmarks/README.md`.

## Concurrent LSP servers

On a polyglot repo the indexer's edge-resolution pass starts one LSP server per
language. By default they run concurrently (up to a cap), so the wall-clock is
closer to the slowest single server than the sum of all of them:

```toml
[lsp]
max_concurrent_servers = 2
```

- Absent or `0` = auto (`min(languages_in_pass, 4)`). `CARTOG_LSP_MAX_SERVERS`
  overrides (env > TOML); `1` forces serial.
- Each server is RAM-heavy (rust-analyzer ~1-2GB resident). Lower the cap on a
  constrained host; most repos have fewer than 4 LSP languages so the cap rarely
  binds.
- Applies to `cartog index` with no live `cartog serve` peer (a bare index). When
  a serve peer holds the DB, the index defers its LSP pass to that warm peer,
  which resolves serially. Resolution output is byte-identical regardless of cap.

**Compile-time feature flags**:

```bash
cargo install cartog                                    # default: LSP + S3 sync + Ollama + OpenAI providers (local ONNX is the runtime default)
cargo install cartog --no-default-features              # minimal: drops LSP, S3 sync, Ollama, and OpenAI
cargo install cartog --no-default-features --features lsp  # selective: LSP only
```
