# How to Wire cartog into Editors

This page is an index for editor integration documentation.

## Automatic setup: `cartog ide`

`cartog ide` writes MCP configuration for all installed editors in one command. See [../usage.md](../usage.md#per-editor-wiring-cartog-ide) for flags and usage.

```bash
cartog ide                    # configure all installed clients
cartog ide --client cursor    # one client only
cartog ide --dry-run          # preview without writing
```

## Manual config recipes (per editor)

For the config-file snippets for every supported MCP client (Claude Code, Claude Desktop, Cursor, Windsurf, OpenCode, Zed, Codex CLI, Gemini CLI, VS Code/Copilot, and the generic stdio pattern):

**[../mcp-setup.md](../mcp-setup.md)**

## Editor-specific integrations (keymaps, tasks, compile-mode)

For Neovim keymaps, VS Code tasks, Emacs `compile`, Telescope picker, and `cartog watch --json` floating buffer:

**[../editor-integration.md](../editor-integration.md)**
