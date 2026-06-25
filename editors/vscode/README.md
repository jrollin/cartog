# cartog for VS Code

Code-graph navigation and semantic search for AI coding agents, inside VS Code.

This extension registers [cartog](https://github.com/jrollin/cartog) as an MCP
server with GitHub Copilot. Copilot's agent can then call cartog's 16 tools —
symbol search, semantic search, references, call-path tracing, impact analysis,
and more — across Python, TypeScript/JavaScript, Rust, Go, Ruby, Java, PHP,
Dart, Swift, Kotlin, Vue, Svelte, Astro, and Markdown.

## What it does

The extension's only job is **delivery**. It finds the `cartog` binary already
on your machine and hands VS Code a stdio MCP server definition for
`cartog serve --watch`. You get the full tool surface in Copilot **without
editing `.vscode/mcp.json`** and without the GUI-launch `PATH` problem that a
hand-written config hits when VS Code is opened from Finder or the Dock.

It ships no binary and adds no new tools — the same `cartog` the CLI runs.

## Requirements

The `cartog` binary must be installed. The extension auto-detects it; if it
isn't found you'll get a one-click prompt with install instructions.

Install cartog (no Rust required):

```bash
curl -fsSL https://jrollin.github.io/cartog/install.sh | sh
```

Or with Cargo:

```bash
cargo install cartog
```

Then index your repo once:

```bash
cartog index .
```

## How the binary is located

In order, the first match wins:

1. The `cartog.binaryPath` setting, if set.
2. `$CARTOG_INSTALL_DIR/cartog`.
3. `cartog` on `PATH`.
4. `~/.local/bin/cartog`, then `~/.cargo/bin/cartog`.

This mirrors the cartog installer, so the extension finds whatever the
installer placed — even when VS Code's process `PATH` is missing your shell's.

## Settings

| Setting | Default | Effect |
|---|---|---|
| `cartog.binaryPath` | `""` (auto-detect) | Absolute path to the `cartog` binary. |
| `cartog.watch` | `true` | Run `cartog serve --watch` (auto-refresh on edits). Set `false` for plain `cartog serve`. |

## Links

- [Documentation](https://jrollin.github.io/cartog/)
- [Usage guide](https://jrollin.github.io/cartog/usage/)
- [Source & issues](https://github.com/jrollin/cartog)

## License

MIT
