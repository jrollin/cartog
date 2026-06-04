# MCP setup (per-editor wiring)

This page documents the config files cartog writes for each supported MCP client. Use it when you want to edit by hand instead of running `cartog ide`.

> **Auto-setup**: `cartog ide` writes these files for you and handles scope (project vs user) and idempotent merging. See [usage.md — `cartog ide`](usage.md#cartog-ide---client-name---scope-projectuserall--y---dry-run---no-watch) for the auto-setup CLI.

All clients need `cartog` on your `PATH` first:

```bash
cargo install cartog             # latest version
cargo install cartog@<version>   # specific version (see Releases)
```

Or use the [install script](../README.md#install) if you don't have a Rust toolchain.

## Claude Code

Claude Code reads MCP servers from two places. `cartog init` writes to the
**project** file; `cartog ide --client claude-code` writes both.

Project-scoped — committable, recommended (`<repo>/.mcp.json`):

```json
{
  "mcpServers": {
    "cartog": {
      "command": "cartog",
      "args": ["serve", "--watch"]
    }
  }
}
```

User-scoped — applies to every repo on your machine (`~/.claude/settings.json`):

```json
{
  "mcpServers": {
    "cartog": {
      "command": "cartog",
      "args": ["serve", "--watch"]
    }
  }
}
```

Or use the Claude CLI directly:

```bash
claude mcp add cartog -- cartog serve --watch              # user scope
claude mcp add --scope project cartog -- cartog serve --watch
```

Add `"--rag"` to `args` if you want automatic embedding updates on file change.

> **Why `--watch` only here?** Of all clients, Claude Code is the only one
> `cartog ide` defaults to `["serve", "--watch"]`; the rest ship plain
> `["serve"]`. Agent-driven workflows churn files faster than human-driven
> editor flows, so the in-process file watcher pays for itself. Drop it with
> `cartog ide --no-watch`, or call `cartog index` manually when you want a
> refresh. Multiple watchers compete for the single-writer lock, so keeping the
> default off elsewhere avoids surprises when a user wires cartog into several
> editors at once.

## Claude Desktop

Edit `~/Library/Application Support/Claude/claude_desktop_config.json` (macOS) or `%APPDATA%\Claude\claude_desktop_config.json` (Windows):

```json
{
  "mcpServers": {
    "cartog": {
      "command": "cartog",
      "args": ["serve"]
    }
  }
}
```

Restart Claude Desktop after editing.

## Cursor

Open Settings > MCP Servers > Add Server:

- **Name**: `cartog`
- **Type**: `command`
- **Command**: `cartog serve`

Or edit `.cursor/mcp.json` in your project root:

```json
{
  "mcpServers": {
    "cartog": {
      "command": "cartog",
      "args": ["serve"]
    }
  }
}
```

## Windsurf

Edit `~/.codeium/windsurf/mcp_config.json`:

```json
{
  "mcpServers": {
    "cartog": {
      "command": "cartog",
      "args": ["serve"]
    }
  }
}
```

## OpenCode

Edit `~/.config/opencode/opencode.json` (or your project's `opencode.json`):

```json
{
  "mcp": {
    "cartog": {
      "type": "local",
      "command": ["cartog", "serve"],
      "enabled": true
    }
  }
}
```

## Zed

Edit `~/.config/zed/settings.json`:

```json
{
  "context_servers": {
    "cartog": {
      "command": "cartog",
      "args": ["serve"]
    }
  }
}
```

## Codex CLI

Edit `~/.codex/config.toml` (Codex reads MCP servers only from this user-global
file — there is no per-project Codex config):

```toml
[mcp_servers.cartog]
command = "cartog"
args = ["serve"]
```

If you use Codex on multiple cartog projects, name each section uniquely
(e.g. `[mcp_servers.cartog-myproj-1a2b3c4d]`) so they coexist. `cartog ide`
does this automatically: section name is `cartog-<slug>-<hash8>`, where
`<slug>` is the project directory name (lowercased, non-alphanumerics → `-`)
and `<hash8>` is the first 4 bytes of SHA-256 of the absolute project path.

## Gemini CLI

Edit `~/.gemini/settings.json`:

```json
{
  "mcpServers": {
    "cartog": {
      "command": "cartog",
      "args": ["serve"]
    }
  }
}
```

## VS Code (GitHub Copilot)

Edit `.vscode/mcp.json` in your project root (per-workspace) — note that
VS Code's top-level key is `servers` (no `Mcp` prefix):

```json
{
  "servers": {
    "cartog": {
      "type": "stdio",
      "command": "cartog",
      "args": ["serve"]
    }
  }
}
```

## Antigravity

Edit `~/.gemini/antigravity/mcp_config.json` (user-global; Antigravity has no
per-project config):

```json
{
  "mcpServers": {
    "cartog": {
      "command": "cartog",
      "args": ["serve"]
    }
  }
}
```

## Kiro

Edit `.kiro/settings/mcp.json` in your project root (workspace scope, takes
precedence) or `~/.kiro/settings/mcp.json` (user scope):

```json
{
  "mcpServers": {
    "cartog": {
      "command": "cartog",
      "args": ["serve"]
    }
  }
}
```

## Hermes Agent

Edit `~/.hermes/config.yaml` (Hermes reads MCP servers from this user-global
YAML file under the `mcp_servers` key):

```yaml
mcp_servers:
  cartog:
    command: cartog
    args:
      - serve
```

`cartog ide` upserts only the `cartog` entry, leaving your other servers and
top-level keys intact. You can also use Hermes' own `hermes mcp add` CLI.

## Any other MCP-compatible client

The config pattern is always the same — point the client at `cartog serve` over stdio:

- **Command**: `cartog`
- **Args**: `["serve"]`
- **Transport**: stdio (default)

## See also

- [usage.md — `cartog ide`](usage.md) — auto-setup CLI (writes these files for you)
- [usage.md — Available MCP tools](usage.md#available-tools) — what each `cartog_*` tool returns
- [editor-integration.md](editor-integration.md) — non-MCP integration (CLI shell-out, quickfix, picker) for Neovim, Emacs, etc.
