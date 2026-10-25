# How to Wire cartog into Editors

This page is an index for editor integration documentation.

## Automatic setup: `cartog ide`

`cartog ide` writes MCP configuration for all installed editors in one command. See [../usage.md](../usage.md#per-editor-wiring-cartog-ide) for flags and usage.

```bash
cartog ide                    # configure all installed clients
cartog ide --client cursor    # one client only
cartog ide --dry-run          # preview without writing
```

## Teach the agent: `--rules`

MCP makes cartog's tools available; a rule file tells the agent to reach for
them instead of grep + read. Add `--rules` to write one:

```bash
cartog install --rules               # every agent in use in this repo
cartog install cursor claude-code --rules
cartog install --rules --dry-run     # preview the files
```

- One file per agent that reads a rules directory: Cursor, Windsurf, Kiro, Cline, Roo Code, Antigravity, Claude Code, VS Code Copilot.
- With no editor named, only agents in use get a file: CLI on `PATH`, user config or app folder, or the rules directory already in the repo.
- The files live in the repo. Commit them so teammates' agents load them too.
- Each file holds a marker line. Re-running refreshes the text; delete the line to keep your own edits.
- A file you wrote yourself at the same path is never touched.

See [../agent-snippet.md](../agent-snippet.md) for the paths and the text, and
[../reference/cli.md](../reference/cli.md#agent-rule-files---rules) for the
cases cartog skips.

## Removing cartog from editors

`cartog uninstall` takes the `cartog` entry out of every config that holds it,
and deletes the rule files `--rules` wrote; `cartog ide --remove` lets you pick
which ones. Other servers, your own files and every folder stay.

```bash
cartog uninstall               # every config in scope that holds cartog
cartog uninstall cursor        # one editor
cartog uninstall --dry-run     # preview the removal
cartog ide --remove            # interactive picker
```

See [../reference/cli.md](../reference/cli.md#cartog-uninstall-client----scope----dry-run) for the full behaviour.

## Manual config recipes (per editor)

For the config-file snippets for every supported MCP client (Claude Code, Claude Desktop, Cursor, Windsurf, OpenCode, Zed, Codex CLI, Gemini CLI, VS Code/Copilot, Antigravity, Kiro, Hermes Agent, Copilot CLI, Qwen Code, Cline, Roo Code, Kilo Code, and the generic stdio pattern):

**[../mcp-setup.md](../mcp-setup.md)**

## Editor-specific integrations (keymaps, tasks, compile-mode)

For Neovim keymaps, VS Code tasks, Emacs `compile`, Telescope picker, and `cartog watch --json` floating buffer:

**[../editor-integration.md](../editor-integration.md)**
