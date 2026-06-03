# Findings: does the agent self-select `cartog_context`? — 2026-06-03

Follow-up to `2026-06-03-findings.md`. That run (curated 8-tool system prompt that
omitted `cartog_context`) showed the cartog arm using ~8× more tokens via many
granular calls. This investigation asked whether the answer-shaped `cartog_context`
tool closes that gap — and corrected two methodology problems along the way.

## Methodology changes made

1. **No system prompt** (both arms) — matching the competitor's A/B method: MCP
   config is the only variable; the model picks tools from their descriptions.
   (The old curated prompt was a confound and hid `cartog_context`.)
2. **Tool fencing** — `--allowedTools` + `--disable-slash-commands` so the ambient
   repo `.claude/` config can't leak the cartog *skill* into the baseline (the
   first no-prompt smoke caught this: baseline had used `Skill`/`Bash`).
3. **`tool_breakdown`** recorded per arm — the `{tool: count}` map of what the model
   actually called, so tool selection is provable, not assumed.
4. **`cartog_context` description rewritten** to be directive ("PRIMARY TOOL — call
   FIRST … Read-equivalent … usually the ONLY call you need") + a funnel: the
   `cartog_rag_search` and `cartog_outline` descriptions now redirect
   "how does X work / understand an area" to `cartog_context`.

All runs: sonnet, N=4, `--no-embed`, parallel. Both arms 4/4 pass throughout.

## Result: the model never self-selects `cartog_context`

cartog-arm `tool_breakdown`, before and after the description rewrite:

| Repo | desc | used `cartog_context`? | other cartog tools | cost vs baseline |
|------|------|------------------------|--------------------|------------------|
| django | old | **no** | outline×3, index×1 | −3.6% |
| django | new | **no** | index×1 | −9.8% |
| tokio | old | **no** | index×1 | −19.7% |
| tokio | new | **no** | map×1, rag_search×1 | +20.7% |

`cartog_context` was **not called once**, in any run, before or after the more
directive description. The agent defaulted to `Read`/`Grep` (django: 19 reads;
tokio: 26 reads). The funnel had a mild, inconsistent effect (tokio picked up
`map`/`rag_search` and improved; django got slightly worse) but did not move the
needle on the target tool.

## But the tool itself is excellent

Called directly on django under `--no-embed` (keyword-only seeds), with the same
question the benchmark asks:

```
$ cartog context "How does the ORM build and execute a SQL query from a QuerySet"
Context for '...' (12 symbols, ~1911 tokens)
[Seed] class ModelIterable  django/db/models/query.py:82
    ...compiler = queryset.query.get_compiler(using=db)
       results = compiler.execute_sql(...)
```

The first seed is `ModelIterable` — the exact bridge symbol the correct answer
needs — and the whole bundle is ~1,900 tokens in one call. An agent that *used*
it would answer in one cheap call instead of 19 file reads. The value is real and
unrealized.

## Conclusion

- The original "8× token blow-up" was largely a **prompt artifact**. Without a
  prompt forcing granular cartog calls, the cartog arm behaves like the baseline
  (reads files), and cost lands within ±20% of baseline.
- **A more directive tool description does not make this model (sonnet, no system
  prompt) self-select `cartog_context`.** Tool quality is not the blocker — the
  tool produces an excellent one-call answer; the model just doesn't reach for it.
- The genuine lever is **guidance** — the SKILL.md / agent instructions real users
  get already say to start with `cartog_context`. The description + funnel changes
  here strengthen that guidance and help prompted usage; they are kept as
  improvements, not because they moved the no-prompt benchmark (they did not).

## Kept changes (this PR)

- `cartog_context` MCP description → directive, Read-displacing framing.
- `cartog_rag_search` / `cartog_outline` descriptions → funnel toward `cartog_context`
  for "how does X work?" / area-understanding tasks.
- `skills/cartog/SKILL.md` context entry kept in sync.
- Harness: no-prompt both arms, tool fencing, `tool_breakdown` recording.

## Not pursued (out of scope / deferred)

- Re-adding a guidance prompt to prove the tool wins *when steered* (re-introduces
  the prompt confound; the direct test already shows the tool is good).
- An `--embed` run (the keyword-only bundle was already excellent, so semantic
  seeds are unlikely to change *selection*).
