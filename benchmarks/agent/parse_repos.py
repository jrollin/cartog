#!/usr/bin/env python3
"""Read the flat repos.yaml schema to JSON, avoiding a pyyaml dependency.

Each repo has scalar fields (id/url/rev/lang) and two folded-block fields
(prompt/expected) whose continuation lines are indented under them.
"""
import json
import sys

SCALARS = ("id", "url", "rev", "lang")
FOLDED = ("prompt", "expected")
KEYS = SCALARS + FOLDED


def strip_fold(value):
    """Drop a leading YAML folded-scalar marker (`>` / `>-`) as a prefix only."""
    v = value.strip()
    if v.startswith(">"):
        v = v[1:].lstrip("-")
    return v.strip()


repos, cur = [], None
with open(sys.argv[1], encoding="utf-8", errors="replace") as fh:
    lines = fh.readlines()

i = 0
while i < len(lines):
    raw = lines[i].rstrip("\n")
    i += 1
    s = raw.strip()
    if not s or s.startswith("#") or s == "repos:":
        continue
    if s.startswith("- id:"):
        if cur:
            repos.append(cur)
        cur = {"id": s.split(":", 1)[1].strip()}
        continue
    if cur is None:
        sys.exit(f"repos.yaml: line {i} '{s}' appears before the first '- id:' entry")
    field = s.split(":", 1)[0]
    if field in SCALARS:
        cur[field] = s.split(":", 1)[1].strip()
    elif field in FOLDED:
        val = strip_fold(s.split(":", 1)[1])
        block = []
        # A continuation is an indented line that does NOT start a new key —
        # stop at a dedent or the next field, not merely at a colon in the text.
        while i < len(lines) and lines[i].startswith("      ") \
                and not lines[i].strip().split(":", 1)[0] in KEYS \
                and not lines[i].strip().startswith("- "):
            block.append(lines[i].strip())
            i += 1
        cur[field] = " ".join([val] + block).strip()

if cur:
    repos.append(cur)

# Every field is required — there is no sensible default for a real-repo target.
# Fail loudly naming the offending entry rather than emitting nulls that would
# clone "null" / leave the judge with no rubric.
REQUIRED = ("id", "url", "rev", "lang", "prompt", "expected")
for idx, repo in enumerate(repos):
    missing = [f for f in REQUIRED if not repo.get(f)]
    if missing:
        name = repo.get("id", f"entry #{idx + 1}")
        sys.exit(f"repos.yaml: '{name}' missing required field(s): {', '.join(missing)}")

print(json.dumps(repos))
