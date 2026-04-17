# Editor integration

Two ways to wire cartog into an editor:

1. **CLI shell-out** — run `cartog refs`, `cartog outline`, etc. from a keymap and show results in a quickfix / picker. Works with any editor. No MCP client required.
2. **MCP server** — run `cartog serve` once, have the editor's AI pane (CodeCompanion, Avante, Cursor, etc.) call cartog tools. This is what Claude Code / Cursor / Zed already use — see [MCP Server Setup](usage.md#mcp-server) for generic client config.

The snippets below prefer `--json` where available so the output is easy to parse.

---

## Neovim

### Prerequisites

```bash
cargo install cartog --features lsp
cartog index .
```

### Quickfix on word-under-cursor (lua)

Drop this anywhere in your config. No plugin dependencies — uses `vim.fn.systemlist` + `vim.fn.setqflist`.

```lua
local function cartog_qf(subcommand)
  return function()
    local word = vim.fn.expand("<cword>")
    if word == "" then return end

    local out = vim.fn.systemlist({ "cartog", subcommand, word, "--json" })
    if vim.v.shell_error ~= 0 then
      vim.notify("cartog " .. subcommand .. " failed: " .. table.concat(out, "\n"),
        vim.log.levels.ERROR)
      return
    end

    local ok, results = pcall(vim.json.decode, table.concat(out, "\n"))
    if not ok or type(results) ~= "table" then
      vim.notify("cartog: unparseable JSON", vim.log.levels.ERROR)
      return
    end

    local items = {}
    for _, r in ipairs(results) do
      -- `refs` / `callees` return arrays of { edge = { file_path, line, ... }, source? }
      local edge = r.edge or r
      table.insert(items, {
        filename = edge.file_path,
        lnum = edge.line,
        text = (r.source and r.source.name or edge.target_name or edge.source_id) or "",
      })
    end

    vim.fn.setqflist(items, "r")
    vim.cmd("copen")
  end
end

vim.keymap.set("n", "<leader>cr", cartog_qf("refs"),    { desc = "cartog: references" })
vim.keymap.set("n", "<leader>cc", cartog_qf("callees"), { desc = "cartog: callees" })
vim.keymap.set("n", "<leader>ci", cartog_qf("impact"),  { desc = "cartog: impact" })
```

### Telescope picker for `cartog search`

If you use `nvim-telescope/telescope.nvim`:

```lua
local pickers      = require("telescope.pickers")
local finders      = require("telescope.finders")
local previewers   = require("telescope.previewers")
local make_entry   = require("telescope.make_entry")
local sorters      = require("telescope.config").values

local function cartog_search()
  local query = vim.fn.input("cartog search > ")
  if query == "" then return end

  local out = vim.fn.systemlist({ "cartog", "search", query, "--json" })
  if vim.v.shell_error ~= 0 then return end
  local ok, results = pcall(vim.json.decode, table.concat(out, "\n"))
  if not ok then return end

  pickers.new({}, {
    prompt_title = "cartog search: " .. query,
    finder = finders.new_table({
      results = results,
      entry_maker = function(r)
        return {
          value    = r,
          display  = string.format("%-12s %s  %s:%d", r.kind, r.name, r.file_path, r.start_line),
          ordinal  = r.name,
          filename = r.file_path,
          lnum     = r.start_line,
        }
      end,
    }),
    sorter   = sorters.generic_sorter({}),
    previewer = previewers.vim_buffer_vimgrep.new({}),
  }):find()
end

vim.keymap.set("n", "<leader>cs", cartog_search, { desc = "cartog: search symbols" })
```

### Live watch with `cartog watch --json`

Stream NDJSON events into a floating buffer so the file tree stays out of the way while tests watch for changes:

```lua
local function cartog_watch()
  local buf = vim.api.nvim_create_buf(false, true)
  local win = vim.api.nvim_open_win(buf, true, {
    relative = "editor", width = 80, height = 12,
    row = vim.o.lines - 14, col = vim.o.columns - 82,
    border = "rounded", title = " cartog watch ",
  })

  vim.fn.jobstart({ "cartog", "watch", ".", "--json" }, {
    stdout_buffered = false,
    on_stdout = function(_, lines)
      if not vim.api.nvim_buf_is_valid(buf) then return end
      vim.api.nvim_buf_set_lines(buf, -1, -1, false,
        vim.tbl_filter(function(l) return l ~= "" end, lines))
    end,
    on_exit = function()
      if vim.api.nvim_win_is_valid(win) then vim.api.nvim_win_close(win, true) end
    end,
  })
end

vim.api.nvim_create_user_command("CartogWatch", cartog_watch, {})
```

### MCP integration (CodeCompanion, Avante)

If your AI plugin supports MCP, point it at `cartog serve`. Example for
[`CodeCompanion`](https://codecompanion.olimorris.dev/):

```lua
require("codecompanion").setup({
  strategies = {
    chat = {
      tools = {
        ["cartog"] = {
          callback = "cartog_mcp",   -- your own tool adapter
        },
      },
    },
  },
})
```

For Avante or other MCP-aware plugins, configure the MCP server the same way you would in Claude Code / Cursor:

```json
{
  "mcpServers": {
    "cartog": {
      "command": "cartog",
      "args": ["serve", "--watch", "--rag"]
    }
  }
}
```

---

## VS Code

### Tasks

Add to `.vscode/tasks.json` so `Ctrl+Shift+P → Tasks: Run Task` can run cartog commands:

```json
{
  "version": "2.0.0",
  "tasks": [
    {
      "label": "cartog: index",
      "type": "shell",
      "command": "cartog index .",
      "problemMatcher": []
    },
    {
      "label": "cartog: refs (word)",
      "type": "shell",
      "command": "cartog refs ${selectedText}",
      "presentation": { "reveal": "always", "panel": "dedicated" }
    }
  ]
}
```

### MCP (Continue, Copilot Chat with MCP extensions)

Same pattern as Claude Code — extensions that speak MCP read a config with an `mcpServers` entry. See [MCP Server Setup in usage.md](usage.md#mcp-server) for the generic block.

---

## Emacs

Compile-mode wrapper:

```elisp
(defun cartog-refs (name)
  "Show cartog refs for NAME in a compilation buffer."
  (interactive (list (read-string "refs: " (thing-at-point 'symbol))))
  (compile (format "cartog refs %s" (shell-quote-argument name))))

(defun cartog-impact (name)
  (interactive (list (read-string "impact: " (thing-at-point 'symbol))))
  (compile (format "cartog impact %s --depth 3" (shell-quote-argument name))))

(global-set-key (kbd "C-c c r") #'cartog-refs)
(global-set-key (kbd "C-c c i") #'cartog-impact)
```

The output format (`path:line  kind  source`) happens to match compilation-mode's default error regex, so `next-error` / `C-x `` navigates the results.

---

## Zed

Already supports cartog via MCP; see [MCP Server Setup](usage.md#mcp-server).

---

## Troubleshooting

- **Command not found** — make sure `~/.cargo/bin` is on `$PATH` or you installed the prebuilt binary (see [README install section](../README.md#install)).
- **Stale results** — if you're using the CLI snippets above, they only see whatever `cartog index` captured. Run `cartog watch .` in a background terminal (or use the floating-buffer lua snippet above) to keep it current.
- **`--json` changes your editor's output handling** — if a picker shows no results, check `vim.v.shell_error` / `$?` first; cartog exits non-zero on bad queries and the body is a JSON error object, not an array.
