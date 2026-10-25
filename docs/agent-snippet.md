# Teach your agent to use cartog

The snippet below tells the LLM to pick cartog tools over grep + read whenever
it needs to navigate code.

## Automatic: `cartog ide --rules`

`cartog install --rules` (or `cartog ide --rules`) writes it as a rule file for
every agent in use that reads a rules directory, with the header that loads it
on every session:

| Agent | Rule file | Header |
|---|---|---|
| Cursor | `.cursor/rules/cartog.mdc` | `alwaysApply: true` |
| Windsurf | `.windsurf/rules/cartog.md` | `trigger: always_on` |
| Kiro | `.kiro/steering/cartog.md` | `inclusion: always` |
| Cline | `.clinerules/cartog.md` | none |
| Roo Code | `.roo/rules/cartog.md` | none |
| Antigravity | `.agents/rules/cartog.md` | `trigger: always_on` (required) |
| Claude Code | `.claude/rules/cartog.md` | none |
| VS Code Copilot | `.github/instructions/cartog.instructions.md` | `applyTo: '**'` |

Each file it writes starts (after the header) with a marker line,
`<!-- Managed by cartog: ... -->`. cartog rewrites or deletes only files that
hold it: a re-run refreshes the text, and `cartog uninstall` deletes the file.
Delete the marker line to take the file over. A file you created yourself at
one of these paths is never touched. See
[reference/cli.md](reference/cli.md#agent-rule-files---rules) for which agents
count as in use and when cartog skips one.

## By hand

For agents that read a single shared file, or when you prefer to own the copy,
paste the snippet into one of:

- `AGENTS.md` (OpenCode, Codex, Aider, any agent that reads it)
- `CLAUDE.md` (Claude Code)
- `.github/copilot-instructions.md` (Copilot)
- any rule file in the table above, with its header

Tool names below are bare. Each client adds its own prefix. In Claude Code:

- `mcp__cartog__cartog_search` after `claude mcp add`
- `mcp__plugin_cartog_cartog__cartog_search` with the plugin

Prerequisite: cartog MCP is wired and the index is built (`cartog ide` + `cartog index .`).

---

## Snippet (copy from here)

```markdown
## Code navigation: prefer cartog over grep

This project is indexed by [cartog](https://github.com/jrollin/cartog). The
cartog MCP tools are pre-computed and return structured, ranked results in
microseconds — far cheaper than grep + read.

**Use cartog when the question is:**
- "where is X defined?" → `cartog_search`
- "who calls / imports / inherits X?" → `cartog_refs`
- "what does X call?" → `cartog_callees`
- "how does X reach Y?" (call path) → `cartog_trace`
- "what breaks if I change X?" → `cartog_impact`
- "show the inheritance tree of X" → `cartog_hierarchy`
- "what does this file import?" → `cartog_deps`
- "show the structure of file F" → `cartog_outline`
- "find code about <concept>" (natural language) → `cartog_rag_search`
- "gather the code for a task" → `cartog_context`
- "orient me in this repo" → `cartog_map`
- "what changed recently?" → `cartog_changes`
- "is the index healthy?" → `cartog_stats`

**Fall back to grep / Read only when:**
- searching for a string literal, comment, config value, or non-code text
- the target file is outside the indexed root
- cartog returned zero results and you need a broader text scan

**Index hygiene:**
- If a tool reports stale data, call `cartog_index` once
- If `cartog_rag_search` results feel out of date after new files were added, call `cartog_rag_index` once
- For semantic search to work, the user must have run `cartog rag setup` and `cartog rag index .`
```

---

## Why this works

- Tells the agent **when** to prefer cartog (the hard part — most agents default to grep)
- Lists the most-used of cartog's 16 MCP tools by their canonical names so the agent calls them directly
- Names the fall-back conditions so the agent doesn't get stuck
- Self-contained — no links to follow at decision time

For the full tool reference, see [usage.md — Available tools](usage.md#available-tools).
