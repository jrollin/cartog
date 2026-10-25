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
