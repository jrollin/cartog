#!/usr/bin/env python3
"""Convert a ContextBench gold parquet into JSONL the evaluator accepts.

The verified-test/verified parquet lacks a `repo_url` column, but
contextbench.parsers.gold._load_parquet hard-codes it and crashes. The JSONL
loader path has no such requirement, so we emit JSONL with repo_url derived from
original_inst_id (owner__repo-N -> https://github.com/owner/repo.git).

Usage:
    python prepare_gold.py --in data/contextbench_verified_test.parquet \
        --out gold.jsonl [--langs go,rust,typescript,java,python]
"""

import argparse
import json
import re

import pyarrow.dataset as ds


def repo_url(orig: str) -> str:
    m = re.match(r"^([A-Za-z0-9_.-]+)__([A-Za-z0-9_.-]+)-\d+$", orig or "")
    return f"https://github.com/{m.group(1)}/{m.group(2)}.git" if m else ""


def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--in", dest="inp", required=True, help="Gold parquet")
    p.add_argument("--out", required=True, help="Output JSONL")
    p.add_argument("--langs", default="", help="Comma-separated language filter")
    args = p.parse_args()

    langs = {x.strip().lower() for x in args.langs.split(",") if x.strip()} or None
    table = ds.dataset(args.inp, format="parquet").to_table()
    written = 0
    skipped_no_url = 0
    with open(args.out, "w", encoding="utf-8") as f:
        for r in table.to_pylist():
            lang = (r.get("language") or "").lower()
            if langs and lang not in langs:
                continue
            url = r.get("repo_url") or repo_url(r.get("original_inst_id") or "")
            if not url:
                skipped_no_url += 1
                continue
            gc = r.get("gold_context")
            try:
                gc = json.loads(gc) if isinstance(gc, str) else (gc or [])
            except json.JSONDecodeError:
                gc = []
            d = {
                "inst_id": r.get("instance_id"),
                "original_inst_id": r.get("original_inst_id"),
                "repo": r.get("repo"),
                "repo_url": url,
                "commit": r.get("base_commit"),
                "gold_ctx": gc,  # Gold falls back to gold_ctx -> init
                "patch": r.get("patch") or "",
                "language": lang,
                "problem_statement": r.get("problem_statement") or "",
            }
            f.write(json.dumps(d, ensure_ascii=False) + "\n")
            written += 1
    print(f"wrote {written} rows to {args.out} (skipped {skipped_no_url} without resolvable repo_url)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
