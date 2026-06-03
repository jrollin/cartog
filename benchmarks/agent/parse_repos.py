#!/usr/bin/env python3
"""Read the flat repos.yaml schema to JSON, avoiding a pyyaml dependency.

Each repo has scalar fields (id/url/rev/lang) and two folded-block fields
(prompt/expected) whose continuation lines are indented under them.
"""
import json
import sys

SCALARS = ("id", "url", "rev", "lang")
FOLDED = ("prompt", "expected")

repos, cur = [], None
with open(sys.argv[1]) as fh:
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
    field = s.split(":", 1)[0]
    if field in SCALARS:
        cur[field] = s.split(":", 1)[1].strip()
    elif field in FOLDED:
        val = s.split(":", 1)[1].strip().lstrip(">").lstrip("-").strip()
        block = []
        while i < len(lines) and lines[i].startswith("      ") and ":" not in lines[i].split("#")[0]:
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
