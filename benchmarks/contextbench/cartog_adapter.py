#!/usr/bin/env python3
"""Adapter: run cartog as the context retriever on ContextBench tasks (Mode A).

For each gold instance: check out the repo at base_commit, index it with cartog,
run `cartog context` (and optionally `cartog rag search`) against the issue's
problem_statement, then emit a ContextBench unified prediction record.

This measures cartog's *retrieval* quality (no LLM edit loop) against
ContextBench's human-annotated gold contexts.

Output is a JSONL consumable directly by:
    python -m contextbench.evaluate --gold <gold.jsonl> --pred preds.jsonl --out results.jsonl

Each record:
    {
      "instance_id": <original_inst_id>,
      "repo_url": <git url>, "commit": <base_commit>,
      "traj_data": {
        "pred_steps": [{"files": [...], "spans": {f: [{start,end}]}, "symbols": {f: [name]}}],
        "pred_files": [...], "pred_spans": {f: [{start,end}]}, "pred_symbols": {f: [name]}
      },
      "model_patch": ""        # Mode A does no editing
    }

NOTE on span precision: cartog's context/rag entries return whole symbol bodies,
including large enclosing containers (modules, big classes). Counting every byte
of a module body as "predicted" destroys span precision. We therefore drop
module-kind spans and cap any single span at MAX_SPAN_LINES; symbol names are kept
regardless so symbol-level recall is unaffected.
"""

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
from collections import defaultdict
from typing import Any, Dict, List, Optional

import pyarrow.dataset as ds

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
# ContextBench checkout helper, used so adapter + evaluator share the repo cache.
# Set CONTEXTBENCH_DIR to the cloned ContextBench repo if it's not on sys.path.
_cb_dir = os.environ.get("CONTEXTBENCH_DIR")
if _cb_dir:
    sys.path.insert(0, _cb_dir)
from contextbench.core import checkout  # noqa: E402

CARTOG_LANGS = {"python", "typescript", "javascript", "rust", "go", "ruby",
                "java", "php", "dart", "swift", "kotlin"}

# Spans larger than this many lines are container bodies (modules / huge classes),
# not localized context; drop them from span/line scoring. Names are still kept.
MAX_SPAN_LINES = 200
# Symbol kinds whose body spans the whole file or a large region — exclude their spans.
CONTAINER_KINDS = {"module", "import"}


def log(msg: str) -> None:
    print(msg, file=sys.stderr, flush=True)


def resolve_repo_url(repo_url: Optional[str], original_inst_id: str) -> str:
    if repo_url:
        return repo_url
    s = (original_inst_id or "").strip()
    m = re.match(r"^([A-Za-z0-9_.-]+)__([A-Za-z0-9_.-]+)-\d+$", s)
    if m:
        return f"https://github.com/{m.group(1)}/{m.group(2)}.git"
    return ""


def run_cartog(args: List[str], cwd: str, db_path: str, timeout: int) -> Optional[Any]:
    cmd = ["cartog", *args, "--json", "--db", db_path]
    try:
        proc = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        log(f"    cartog {args[0]} timed out")
        return None
    if proc.returncode != 0:
        log(f"    cartog {args[0]} failed (rc={proc.returncode}): {proc.stderr.strip()[:200]}")
        return None
    out = proc.stdout.strip()
    if not out:
        return None
    try:
        return json.loads(out)
    except json.JSONDecodeError:
        log(f"    cartog {args[0]} non-JSON output: {out[:120]}")
        return None


def index_repo(repo_dir: str, db_path: str, timeout: int) -> bool:
    cmd = ["cartog", "index", repo_dir, "--db", db_path]
    try:
        proc = subprocess.run(cmd, cwd=repo_dir, capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        log("    cartog index timed out")
        return False
    if proc.returncode != 0:
        log(f"    cartog index failed (rc={proc.returncode}): {proc.stderr.strip()[:200]}")
        return False
    return True


def rag_index_repo(repo_dir: str, db_path: str, timeout: int) -> bool:
    """Build the embedding index so vector search (semantic) is live.

    PATH must be the repo: `rag index` re-walks it before embedding, and a
    mismatched cwd (e.g. the run dir) makes the sweep delete every indexed file.
    """
    cmd = ["cartog", "rag", "index", repo_dir, "--db", db_path]
    try:
        proc = subprocess.run(cmd, cwd=repo_dir, capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        log("    cartog rag index timed out")
        return False
    if proc.returncode != 0:
        log(f"    cartog rag index failed (rc={proc.returncode}): {proc.stderr.strip()[:200]}")
        return False
    return True


_IDENT_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_]{2,}")
_STOPWORDS = {
    "the", "and", "for", "with", "that", "this", "from", "have", "has", "not",
    "but", "all", "any", "are", "was", "were", "when", "then", "than", "also",
    "should", "would", "could", "doesn", "don", "isn", "you", "your", "they",
    "them", "their", "its", "it's", "however", "related", "issue", "bug", "fix",
    "works", "work", "working", "expected", "expect", "example", "https", "http",
    "com", "github", "codesandbox", "great", "awesome", "looks", "look", "seem",
    "seems", "apply", "applied", "passing", "pass", "change", "changes", "perspective",
}


def keyword_query(problem: str, max_terms: int = 12) -> str:
    """Extract a keyword/identifier query from issue prose.

    cartog's FTS path drowns in prose: stopword-heavy queries rank markdown docs
    above code, which the default CodeOnly filter then drops. Prefer code-like
    tokens: backticked spans, CamelCase, snake_case, and distinctive identifiers.
    """
    # 1. Backticked / quoted code spans get top priority.
    spans = re.findall(r"`([^`]+)`", problem)
    terms: List[str] = []
    seen: set = set()

    def add(tok: str) -> None:
        t = tok.strip()
        low = t.lower()
        if not t or low in seen or low in _STOPWORDS:
            return
        seen.add(low)
        terms.append(t)

    for s in spans:
        for tok in _IDENT_RE.findall(s):
            add(tok)

    # 2. CamelCase / snake_case identifiers in the title (first line) and body.
    title = problem.splitlines()[0] if problem else ""
    for region in (title, problem):
        for tok in _IDENT_RE.findall(region):
            if any(c.isupper() for c in tok[1:]) or "_" in tok:  # camelCase / snake_case
                add(tok)
            if len(terms) >= max_terms:
                break

    # 3. Backfill with remaining distinctive words if we are short.
    if len(terms) < 4:
        for tok in _IDENT_RE.findall(title or problem):
            add(tok)
            if len(terms) >= max_terms:
                break

    return " ".join(terms[:max_terms])


def symbols_to_step(entries: List[dict]) -> Dict[str, Any]:
    """Convert cartog context/search entries into a unified step.

    Names are always recorded (symbol recall). Spans are recorded only for
    localized, non-container symbols (span/line precision).
    """
    files: set = set()
    spans: Dict[str, List[dict]] = defaultdict(list)
    symbols: Dict[str, List[str]] = defaultdict(list)
    for e in entries:
        sym = e.get("symbol", e)  # rag search may put symbol fields at top level
        fp = sym.get("file_path") or sym.get("file")
        if not fp:
            continue
        files.add(fp)
        name = sym.get("name")
        if name:
            symbols[fp].append(name)
        start = sym.get("start_line")
        end = sym.get("end_line")
        kind = (sym.get("kind") or "").lower()
        if not (isinstance(start, int) and isinstance(end, int) and start > 0 and end >= start):
            continue
        if kind in CONTAINER_KINDS:
            continue
        if (end - start + 1) > MAX_SPAN_LINES:
            continue
        spans[fp].append({"start": start, "end": end})
    return {
        "files": sorted(files),
        "spans": {f: v for f, v in spans.items() if v},
        "symbols": {f: sorted(set(v)) for f, v in symbols.items()},
    }


def merge_steps(steps: List[Dict[str, Any]]) -> Dict[str, Any]:
    files: set = set()
    spans: Dict[str, List[dict]] = defaultdict(list)
    symbols: Dict[str, List[str]] = defaultdict(list)
    for st in steps:
        files.update(st.get("files", []))
        for f, v in st.get("spans", {}).items():
            spans[f].extend(v)
        for f, v in st.get("symbols", {}).items():
            symbols[f].extend(v)
    # Dedup spans per file.
    dedup_spans: Dict[str, List[dict]] = {}
    for f, v in spans.items():
        seen = set()
        uniq = []
        for s in v:
            key = (s["start"], s["end"])
            if key in seen:
                continue
            seen.add(key)
            uniq.append(s)
        dedup_spans[f] = uniq
    return {
        "files": sorted(files),
        "spans": dedup_spans,
        "symbols": {f: sorted(set(v)) for f, v in symbols.items()},
    }


def build_prediction(row: dict, cache_dir: str, tokens: int, use_rag: bool,
                     idx_timeout: int, q_timeout: int, build_embeddings: bool,
                     use_keyword: bool) -> Optional[dict]:
    instance_id = row.get("instance_id") or row.get("original_inst_id")
    original = row.get("original_inst_id") or instance_id
    commit = row.get("base_commit") or row.get("commit")
    language = (row.get("language") or "").lower()
    problem = (row.get("problem_statement") or "").strip()

    if language not in CARTOG_LANGS:
        log(f"  SKIP {instance_id}: language '{language}' unsupported by cartog")
        return None
    if not problem:
        log(f"  SKIP {instance_id}: empty problem_statement")
        return None

    repo_url = resolve_repo_url(row.get("repo_url"), original)
    if not repo_url or not commit:
        log(f"  SKIP {instance_id}: missing repo_url/commit")
        return None

    log(f"  {instance_id} [{language}] {repo_url}@{commit[:10]}")
    repo_dir = checkout(repo_url, commit, cache_dir, verbose=False)
    if not repo_dir or not os.path.isdir(repo_dir):
        log("    checkout failed")
        return None

    # Cache the DB by repo+commit so re-runs and repeated commits skip re-indexing.
    slug = re.sub(r"[^A-Za-z0-9]+", "_", f"{original}_{commit[:10]}")
    db_path = os.path.join(tempfile.gettempdir(), f"cartog_cb_{slug}.db")
    if not os.path.exists(db_path):
        if not index_repo(repo_dir, db_path, idx_timeout):
            return None
        if build_embeddings:
            log("    building embeddings (rag index)")
            rag_index_repo(repo_dir, db_path, idx_timeout)  # best-effort; FTS still works if it fails

    query = keyword_query(problem) if use_keyword else problem[:2000]
    log(f"    query: {query!r}")
    steps: List[Dict[str, Any]] = []

    ctx = run_cartog(["context", query, "--tokens", str(tokens)], repo_dir, db_path, q_timeout)
    if ctx and ctx.get("entries"):
        steps.append(symbols_to_step(ctx["entries"]))

    if use_rag:
        rag = run_cartog(["rag", "search", query], repo_dir, db_path, q_timeout)
        if rag:
            results = rag.get("results") or rag.get("entries") or (rag if isinstance(rag, list) else [])
            if isinstance(results, list) and results:
                steps.append(symbols_to_step(results))

    if not steps:
        log("    no cartog context produced")
        return None

    final = merge_steps(steps)
    return {
        "instance_id": original,
        "repo_url": repo_url,
        "commit": commit,
        "traj_data": {
            "pred_steps": steps,
            "pred_files": final["files"],
            "pred_spans": final["spans"],
            "pred_symbols": final["symbols"],
        },
        "model_patch": "",
    }


def repo_url_from_id(orig: str) -> str:
    return resolve_repo_url(None, orig)


def load_gold_rows(gold_path: str, limit: int, langs: Optional[set]) -> List[dict]:
    """Load gold rows from a parquet or JSONL file into adapter-ready dicts."""
    rows: List[dict] = []
    if gold_path.endswith(".parquet"):
        table = ds.dataset(gold_path, format="parquet").to_table()
        for r in table.to_pylist():
            rows.append({
                "instance_id": r.get("instance_id"),
                "original_inst_id": r.get("original_inst_id"),
                "repo": r.get("repo"),
                "repo_url": r.get("repo_url"),
                "base_commit": r.get("base_commit"),
                "language": r.get("language"),
                "problem_statement": r.get("problem_statement"),
            })
    else:
        with open(gold_path, encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if line:
                    rows.append(json.loads(line))
    if langs:
        rows = [r for r in rows if (r.get("language") or "").lower() in langs]
    if limit and limit > 0:
        rows = rows[:limit]
    return rows


def main() -> int:
    p = argparse.ArgumentParser(description="cartog-as-retriever adapter for ContextBench")
    p.add_argument("--gold", required=True, help="Gold parquet or JSONL path")
    p.add_argument("--out", required=True, help="Output predictions JSONL")
    p.add_argument("--cache", default="./repos", help="Repo cache dir (shared with evaluator)")
    p.add_argument("--limit", type=int, default=0, help="Max instances (0 = all)")
    p.add_argument("--langs", default="", help="Comma-separated language filter")
    p.add_argument("--tokens", type=int, default=6000, help="cartog context token budget")
    p.add_argument("--no-rag", action="store_true", help="Skip cartog rag search step")
    p.add_argument("--rag-index", action="store_true",
                   help="Build embeddings (cartog rag index) so vector search is live; requires `cartog rag setup`")
    p.add_argument("--keyword-query", action="store_true",
                   help="Extract code identifiers from the issue text instead of feeding raw prose")
    p.add_argument("--index-timeout", type=int, default=600)
    p.add_argument("--query-timeout", type=int, default=120)
    args = p.parse_args()

    langs = {x.strip().lower() for x in args.langs.split(",") if x.strip()} or None
    rows = load_gold_rows(args.gold, args.limit, langs)
    log(f"Loaded {len(rows)} gold instances")

    os.makedirs(os.path.dirname(args.out) or ".", exist_ok=True)
    written = 0
    with open(args.out, "w", encoding="utf-8") as f:
        for i, row in enumerate(rows, 1):
            log(f"[{i}/{len(rows)}]")
            pred = build_prediction(row, args.cache, args.tokens, not args.no_rag,
                                    args.index_timeout, args.query_timeout,
                                    args.rag_index, args.keyword_query)
            if pred:
                f.write(json.dumps(pred, ensure_ascii=False) + "\n")
                f.flush()
                written += 1
    log(f"Wrote {written} predictions to {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
