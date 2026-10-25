# Teach your agent to use cartog

Copy the snippet below into your project's agent rules file so the LLM picks
cartog tools over grep + read whenever it needs to navigate code.

Drop it into one of:

- `AGENTS.md` (OpenCode, Codex, Aider, any agent that reads it)
- `CLAUDE.md` (Claude Code)
- `.cursor/rules/cartog.mdc` (Cursor, with frontmatter `alwaysApply: true`)
- `.windsurf/rules/cartog.md` (Windsurf, with frontmatter `trigger: always_on`)
- `.kiro/steering/cartog.md` (Kiro)
- `.clinerules/cartog.md` (Cline)
- `.roo/rules/cartog.md` (Roo Code)
- `.github/copilot-instructions.md` (Copilot)

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
