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

_Mode A, verified-test subset, 38 instances (2026-06-10), `--rag-index
--keyword-query --tokens 6000`. Macro-averaged (mean of per-instance scores)
per language; the evaluator's pooled overall is shown below the table._

| Language   | n  | File cov | File ceiling | File prec | Span cov | Span prec | Line cov |
|------------|----|----------|--------------|-----------|----------|-----------|----------|
| rust       | 3  | 1.000    | 1.000        | 0.117     | 0.534    | 0.254     | 0.511    |
| go         | 3  | 0.833    | 0.917        | 0.126     | 0.314    | 0.135     | 0.256    |
| java       | 4  | 0.596    | 0.929        | 0.392     | 0.075    | 0.404     | 0.063    |
| typescript | 6  | 0.533    | 0.833        | 0.101     | 0.168    | 0.071     | 0.210    |
| python     | 22 | 0.504    | 1.000        | 0.103     | 0.277    | 0.075     | 0.256    |
| **ALL**    | 38 | 0.583    | 0.960        | 0.136     | 0.262    | 0.128     | 0.249    |

Evaluator pooled overall: file 0.500/0.118, span 0.144/0.091, line 0.134/0.091
(coverage/precision), EditLoc recall 0.013.

### Ablation: FTS-only (vector search off)

_Same harness and instances, `--keyword-query` only (no `--rag-index`), all 38
indexed `--no-lsp`. Measures what vector search adds on top of keyword FTS._

| Language   | n  | File cov | File prec | Span cov | Span prec | Line cov |
|------------|----|----------|-----------|----------|-----------|----------|
| rust       | 3  | 1.000    | 0.172     | 0.536    | 0.319     | 0.513    |
| go         | 3  | 0.833    | 0.117     | 0.366    | 0.131     | 0.302    |
| python     | 22 | 0.519    | 0.109     | 0.261    | 0.079     | 0.242    |
| typescript | 6  | 0.406    | 0.093     | 0.084    | 0.071     | 0.118    |
| java       | 4  | 0.400    | 0.363     | 0.068    | 0.508     | 0.056    |
| **ALL**    | 38 | 0.551    | 0.138     | 0.243    | 0.146     | 0.229    |

Evaluator pooled overall: file 0.477/0.115, span 0.139/0.094, line 0.130/0.096,
EditLoc recall 0.013.

Hybrid minus FTS-only, file coverage: java **+0.196**, typescript **+0.127**
(plus 2× span coverage there), rust/go **0.000** (identifier-rich issues —
keywords already saturate), python **−0.015** (a wash, within noise). Vector
search pays where issue prose and code vocabulary diverge (java, typescript)
and costs nothing elsewhere. Caveat: the ablation ran `--no-lsp` on all 38
while the hybrid run had LSP on 34, so the deltas bundle both effects — but
LSP only changes 1-hop neighbor expansion (heuristics resolve ~90% of edges),
so the vector effect dominates.

Caveats for reading these numbers:

- **Symbol-level metrics are omitted — they are vacuous on this dataset.** The
  evaluator (under the `tree-sitter-language-pack` fallback) extracted zero
  gold symbols on every one of the 38 instances, so symbol coverage/precision
  read 1.000/1.000 by the 0/0 convention and carry no information.
- **Single-shot retrieval, not an agent.** One query per instance, no
  iteration, 6000-token budget; gold contexts are annotated from solutions and
  often exceed the budget. Expect coverage ceilings well below multi-turn
  agents.
- **Recall-first by design**: low file precision is the cost of bundling
  1-hop graph neighbors; an agent can skim past extras, but it cannot use
  files that were never retrieved.
- **File ceiling** is the max achievable file coverage: 5 of 86 gold files
  are unreachable in principle (build/config files cartog doesn't index —
  Makefile, 2× package.json, pom.xml — plus one issue-attached reproducer
  outside the repo tree). rust/go sit at their ceiling; the
  python/typescript gaps are genuine retrieval headroom.
- **Java span coverage (0.075) understates retrieval there**: java gold
  spans are whole-class regions (often 300+ lines from line 1, imports and
  class header included) while cartog returns method-level spans — java's
  span *precision* (0.404) is the highest of any language. Right
  neighborhoods, small slices.
- 34 instances were indexed with LSP edge resolution; 4 oversized repos
  (transformers ×2, django-11603, material-ui-32626) used `--no-lsp` after
  blowing the index timeout. This affects only 1-hop neighbor expansion,
  not search seeds.
- n is tiny for rust/go/java — single instances move those means a lot.
