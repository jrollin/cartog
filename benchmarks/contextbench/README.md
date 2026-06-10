# ContextBench eval (cartog-as-retriever)

[ContextBench](https://github.com/EuniAI/ContextBench) (arXiv 2602.05892) is a
process-oriented benchmark for **context retrieval** in coding agents: 1,136
issue-resolution tasks across 66 repos and 8 languages, each with a
human-annotated gold context. It scores how well an agent *finds the right code*
(file / symbol / span / line coverage + precision), not whether a patch passes
tests.

This directory wires **cartog as the retriever** ("Mode A"): for each task we
check out the repo at its base commit, index it with cartog, run `cartog context`
+ `cartog rag search` on the issue text, and emit a ContextBench prediction. This
measures cartog's retrieval quality directly, with no LLM edit loop.

## Files

| File | Purpose |
|------|---------|
| `cartog_adapter.py` | Run cartog over gold instances → ContextBench unified predictions JSONL |
| `prepare_gold.py` | Convert a gold parquet → JSONL the evaluator accepts (works around the loader bug below) |

## Setup

ContextBench pins `tree-sitter==0.20.4` and `tree-sitter-languages`, which do not
build/install on Python ≥ 3.12. Use the modern stack instead — the ContextBench
code already falls back to `tree_sitter_language_pack`:

```bash
git clone https://github.com/EuniAI/ContextBench /tmp/ContextBench
cd /tmp/ContextBench
python3 -m venv .venv
.venv/bin/pip install pyarrow pandas tree-sitter tree-sitter-language-pack
# datasets live in data/*.parquet (verified subset, full set)
```

## Run

```bash
export CONTEXTBENCH_DIR=/tmp/ContextBench   # so the adapter can import contextbench.core.checkout

# 1. Prepare gold JSONL (filter to cartog-supported languages)
.venv/bin/python prepare_gold.py \
    --in data/contextbench_verified_test.parquet \
    --out gold.jsonl \
    --langs go,rust,typescript,java,python

# 2. Run cartog as the retriever (clones + indexes each repo; slow on large repos)
#    --rag-index    builds embeddings so vector search is live (needs `cartog rag setup`)
#    --keyword-query extracts code identifiers from issue prose (see "prose query" gotcha)
.venv/bin/python cartog_adapter.py \
    --gold gold.jsonl --out preds.jsonl --cache ./repos \
    --rag-index --keyword-query \
    --tokens 6000 --index-timeout 1200 --query-timeout 240

# 3. Score against gold
.venv/bin/python -m contextbench.evaluate \
    --gold gold.jsonl --pred preds.jsonl --cache ./repos --out results.jsonl
```

The summary (file/symbol/span/line Coverage + Precision, plus trajectory AUC and
EditLoc) is printed to stderr; per-instance breakdowns go to `results.jsonl`.

## Gotchas (learned the hard way)

- **Feed gold as JSONL, not parquet.** ContextBench's parquet loader
  (`contextbench/parsers/gold.py::_load_parquet`) hard-codes a `repo_url` column
  that the verified-test/verified parquet does not have, and crashes with
  `ArrowInvalid: No match for FieldRef.Name(repo_url)`. `prepare_gold.py` emits
  JSONL with `repo_url` derived from `original_inst_id`
  (`owner__repo-N` → `https://github.com/owner/repo.git`), which the JSONL loader
  path accepts.
- **Python ≥ 3.12 dep wall.** `tree-sitter==0.20.4` / `tree-sitter-languages` from
  `requirements.txt` won't install; use `tree-sitter-language-pack` (see Setup).
- **Span precision is sensitive to what cartog emits.** cartog's `context`/`rag`
  entries return whole symbol bodies, including large containers (modules, big
  classes). Counting every byte of a module body as "predicted" tanks span/line
  precision. The adapter drops `module`/`import`-kind spans and caps any single
  span at `MAX_SPAN_LINES` (default 200); symbol *names* are kept regardless, so
  symbol-level recall is unaffected.
- **Empty gold symbols.** Some gold contexts (often Python) annotate file-header /
  import regions that don't overlap a tree-sitter `function`/`class` node, so
  ContextBench extracts zero gold symbols. Symbol-level Coverage/Precision then
  reads 1.000/1.000 vacuously — treat file + span + EditLoc as the trustworthy
  axes for those instances.
- **Wall-clock is dominated by clone+index** of large repos (django, transformers,
  cli). Scope runs by language and/or `--limit`; repos are cached under `--cache`
  and shared with the evaluator. The adapter caches the cartog DB by repo+commit,
  so re-runs and repeated commits skip re-indexing.
- **Feed cartog a keyword query, not raw issue prose** (`--keyword-query`). With
  vector search off, a long prose query's stopword-heavy tokens
  (`changes`, `however`, `Related`) match **markdown** symbols (CHANGELOG, docs)
  far more than code. FTS ranks those docs top, then cartog's default `CodeOnly`
  filter drops every `Document` → **0 results** ("no cartog context produced").
  Empirically, on the same indexed DB, raw prose → 0 results, 12 code identifiers
  → 6+ results. `--keyword-query` extracts backticked spans, CamelCase, and
  snake_case identifiers from the issue text. `--rag-index` (vector search on)
  also rescues prose queries.
- **`cartog rag setup` is a prerequisite for `--rag-index`.** Without embeddings,
  `vec_count=0` and only FTS keyword matching is live — which is what made the
  first run's TypeScript coverage collapse.

## Interpreting results

cartog is built for **high recall** retrieval (hybrid search + 1-hop graph
neighbors + centrality), so expect strong file/span **coverage** and lower
**precision** — exactly the recall-over-precision tradeoff the ContextBench paper
identifies across agents. The honest way to report cartog is a coverage-precision
table per language, optionally sweeping `--tokens` to trace the tradeoff curve.

## Results

_Mode A, verified-test subset (go/rust/typescript/java/python). Populated from a
real run — see `results.jsonl`._

<!-- RESULTS_TABLE -->
